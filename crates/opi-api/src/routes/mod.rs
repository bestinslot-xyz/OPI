//! The app's routes, registered in this order: `/v1/status` and
//! `/v1/openapi.json`, then each module's routes when it is enabled, or a
//! 404 for its whole prefix when it is not, then finalhandler's 404 for
//! anything else.
//!
//! Every route is registered with `.routes(routes!(handler))`, which adds
//! the handler's `#[utoipa::path]` operation to the OpenAPI document (see
//! [`openapi`](crate::openapi)). A module registers through
//! [`ModuleRoutes`], which takes nothing else; the disabled modules'
//! catch-alls here are plain routes, outside the document.

mod bitmap;
mod brc20;
pub mod paging;
mod registry;
mod runes;
mod sns;

pub use registry::ModuleRoutes;

use {
    crate::{
        config::{MODULES, Module},
        db::Db,
        js::{JsError, JsObject, JsValue},
        js_object,
        openapi::{self, Answer, ApiDoc, DbHeight, SpecJson},
        reply::{Reply, not_found},
    },
    axum::{
        Router,
        extract::{OriginalUri, State},
        http::Method,
        response::Response,
        routing::any,
    },
    futures::{StreamExt, stream::FuturesUnordered},
    serde_json::json,
    std::{collections::BTreeMap, sync::Arc, time::Duration},
    utoipa::{OpenApi, PartialSchema, ToSchema, openapi::RefOr, openapi::schema::Schema},
    utoipa_axum::{
        router::{OpenApiRouter, UtoipaMethodRouterExt},
        routes,
    },
};

/// `TypeError: Cannot read properties of undefined (reading '<field>')`,
/// which `res.rows[0].<field>` throws on an empty result.
fn no_row(field: &str) -> JsError {
    JsError::type_error(format!(
        "Cannot read properties of undefined (reading '{field}')"
    ))
}

/// The API settings routes run with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteSettings {
    /// How long a holders snapshot may stream (API_SNAPSHOT_TIMEOUT_MS).
    pub snapshot_timeout: Duration,
}

impl Default for RouteSettings {
    fn default() -> Self {
        RouteSettings {
            snapshot_timeout: Duration::from_millis(600_000),
        }
    }
}

/// A module's route registration: the module's routes, which read from
/// `db`, with their operations.
pub type RouteModule = fn(db: &Db, settings: &RouteSettings) -> ModuleRoutes;

/// The registration function of `module`.
pub fn module_routes(module: Module) -> RouteModule {
    match module {
        Module::Brc20 => brc20::routes,
        Module::Bitmap => bitmap::routes,
        Module::Sns => sns::routes,
        Module::Runes => runes::routes,
    }
}

/// The routes for the `enabled` modules, each reading from its entry in
/// `dbs`. An enabled module without an entry gets a database whose every
/// query fails, as a missing pool makes every query throw in the app.
pub fn routes(enabled: &[Module], dbs: Vec<(Module, Db)>, settings: &RouteSettings) -> Router {
    app(enabled, dbs, settings).0
}

/// The routes of [`routes`] and the OpenAPI document of their operations,
/// which `/v1/openapi.json` serves.
pub fn app(
    enabled: &[Module],
    dbs: Vec<(Module, Db)>,
    settings: &RouteSettings,
) -> (Router, utoipa::openapi::OpenApi) {
    let db_of = |module: Module| {
        dbs.iter()
            .find(|(m, _)| *m == module)
            .map(|(_, db)| db.clone())
            .unwrap_or_else(|| {
                Db::fake(move |_| anyhow::bail!("no database for {}", module.name()))
            })
    };
    let status_dbs: Vec<(Module, Db)> = enabled.iter().map(|&m| (m, db_of(m))).collect();
    let spec = SpecJson::default();
    let mut router = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(status).with_state(Arc::new(status_dbs)))
        .routes(routes!(openapi::openapi_json).with_state(spec.clone()));
    for module in MODULES {
        if enabled.contains(&module) {
            let db = db_of(module);
            let module_router = module_routes(module)(&db, settings).into_inner();
            let schemas: Vec<_> = module_router
                .get_openapi()
                .components
                .iter()
                .flat_map(|components| components.schemas.clone())
                .collect();
            registry::check_components(router.get_openapi(), &schemas);
            router = router.merge(module_router.with_state(db));
        } else {
            let prefix = format!("/v1/{}", module.name());
            let disabled = move || async move {
                Reply::send_json(js_object! {
                    "error" => format!("{} is not enabled on this node", module.name()),
                })
                .status(404)
            };
            // `app.use(prefix)` matches the prefix, `prefix/`, and anything
            // under it; routing strips one trailing slash, so `prefix//`
            // arrives as `prefix/`
            router = router
                .route(&prefix, any(disabled))
                .route(&format!("{prefix}/"), any(disabled))
                .route(&format!("{prefix}/{{*rest}}"), any(disabled));
        }
    }
    let (router, mut api) = router.split_for_parts();
    openapi::finish(&mut api);
    spec.set(api.to_json().expect("the document serializes"))
        .expect("the document is set once");
    let router = router
        .fallback(express_404)
        .method_not_allowed_fallback(express_404);
    (router, api)
}

/// finalhandler's 404, naming the method and the path the client sent.
async fn express_404(method: Method, OriginalUri(uri): OriginalUri) -> Response {
    not_found(method.as_str(), uri.path())
}

/// The `/v1/status` result: an entry per enabled module, by name.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StatusResult {
    modules: BTreeMap<String, StatusEntry>,
}

/// A module's status: its latest indexed height, or
/// `{"error":"unavailable"}` when the height query fails.
struct StatusEntry;

impl PartialSchema for StatusEntry {
    fn schema() -> RefOr<Schema> {
        openapi::json_schema(json!({
            "description": "A module's latest indexed height, or `{\"error\":\"unavailable\"}` when its height query fails (the cause is logged).",
            "oneOf": [
                {
                    "type": "object",
                    "required": ["block_height"],
                    "properties": {"block_height": DbHeight::schema()},
                },
                {
                    "type": "object",
                    "required": ["error"],
                    "properties": {"error": {"type": "string", "enum": ["unavailable"]}},
                },
            ],
        }))
    }
}

impl ToSchema for StatusEntry {}

/// Each enabled module's latest indexed height.
///
/// The modules are listed in the order their height queries answer.
#[utoipa::path(
    get,
    path = "/v1/status",
    tag = "api",
    responses((status = 200, description = "The enabled modules", body = Answer<StatusResult>)),
)]
async fn status(State(dbs): State<Arc<Vec<(Module, Db)>>>) -> Reply {
    // The modules are queried together and listed in the order their
    // answers arrive, as the app's `Promise.all` callbacks assign them.
    let mut pending: FuturesUnordered<_> = dbs
        .iter()
        .map(|(module, db)| async move { (*module, db.latest_block_height(*module).await) })
        .collect();
    let mut modules = JsObject::new();
    while let Some((module, height)) = pending.next().await {
        let entry = match height {
            Ok(height) => js_object! { "block_height" => height },
            Err(err) => {
                tracing::error!("status query failed for {}: {err:#}", module.name());
                js_object! { "error" => "unavailable" }
            }
        };
        modules.set(module.name(), entry);
    }
    Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! { "modules" => modules },
    })
}
