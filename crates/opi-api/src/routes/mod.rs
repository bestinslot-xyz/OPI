//! The app's routes, registered in the Node API's order: `/v1/status`, then
//! each module's routes when it is enabled, or a 404 for its whole prefix
//! when it is not, then finalhandler's 404 for anything else.

mod bitmap;
mod brc20;
mod runes;
mod sns;

use {
    crate::{
        config::{MODULES, Module},
        db::Db,
        js::{JsError, JsObject, JsValue},
        js_object,
        reply::{Reply, not_found},
    },
    axum::{
        Router,
        extract::{OriginalUri, State},
        http::Method,
        response::Response,
        routing::{any, get},
    },
    futures::{StreamExt, stream::FuturesUnordered},
    std::sync::Arc,
};

/// `TypeError: Cannot read properties of undefined (reading '<field>')`,
/// which `res.rows[0].<field>` throws on an empty result.
fn no_row(field: &str) -> JsError {
    JsError::type_error(format!(
        "Cannot read properties of undefined (reading '{field}')"
    ))
}

/// A module's route registration: adds the module's routes, which read
/// from `db`, to `router`.
pub type RouteModule = fn(router: Router, db: Db) -> Router;

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
pub fn routes(enabled: &[Module], dbs: Vec<(Module, Db)>) -> Router {
    let db_of = |module: Module| {
        dbs.iter()
            .find(|(m, _)| *m == module)
            .map(|(_, db)| db.clone())
            .unwrap_or_else(|| {
                Db::fake(move |_| anyhow::bail!("no database for {}", module.name()))
            })
    };
    let status_dbs: Vec<(Module, Db)> = enabled.iter().map(|&m| (m, db_of(m))).collect();
    let mut router =
        Router::new().route("/v1/status", get(status).with_state(Arc::new(status_dbs)));
    for module in MODULES {
        if enabled.contains(&module) {
            router = module_routes(module)(router, db_of(module));
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
    router
        .fallback(express_404)
        .method_not_allowed_fallback(express_404)
}

/// finalhandler's 404, naming the method and the path the client sent.
async fn express_404(method: Method, OriginalUri(uri): OriginalUri) -> Response {
    not_found(method.as_str(), uri.path())
}

/// `/v1/status`: each enabled module's latest indexed height, or
/// `{"error":"unavailable"}` when its query fails. The modules are queried
/// together and listed in the order their answers arrive, as the app's
/// `Promise.all` callbacks assign them.
async fn status(State(dbs): State<Arc<Vec<(Module, Db)>>>) -> Reply {
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
