//! The API's OpenAPI 3.1 document, served at `/v1/openapi.json`.
//!
//! A route and its operation are registered in one call: each handler
//! carries a `#[utoipa::path]` attribute naming its path, parameters and
//! answers, and its module adds it with `.routes(routes!(handler))` on an
//! [`OpenApiRouter`](utoipa_axum::router::OpenApiRouter), which routes the
//! path and records the operation. [`routes::app`](crate::routes::app) builds
//! the router and the document from the same calls, so the document lists the
//! routes of the enabled modules, `/v1/status` and `/v1/openapi.json`.
//!
//! A handler's doc comment is its operation's text: the first paragraph is
//! the summary (the README's route tables show it), the rest the
//! description. [`finish`] adds the answers every operation shares: the 500
//! body and the rate limiter's 429.
//!
//! The schema types here and next to the handlers describe the JSON the
//! routes write through [`js`](crate::js) values. They are documentation
//! types: no route builds them.

#[cfg(test)]
mod tests;

use {
    crate::reply,
    axum::{
        extract::State,
        http::{HeaderValue, header},
        response::{IntoResponse, Response},
    },
    serde_json::json,
    std::sync::{Arc, OnceLock},
    utoipa::{
        IntoParams, OpenApi, PartialSchema, ToSchema,
        openapi::{
            self, ContentBuilder, HeaderBuilder, RefOr, ResponseBuilder,
            path::{HttpMethod, Operation},
            schema::Schema,
        },
    },
};

/// The document's info and tags; the routes add its paths.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "OPI API",
        description = "The HTTP API of opi-indexer: the BRC-20, Bitmap, SNS and Runes modules' routes under `/v1/<module>/`. The routes of the Node API answer as it does (node-pg value forms included); the ticker, wallet and lookup routes answer `{\"error\":null,\"result\":...}`. Every route answers `500 {\"error\":\"internal error\",\"result\":null}` when its handler fails, and `429` when RATE_LIMIT_ENABLE is `true` and the client has used up its window.",
        license(name = "Apache-2.0", identifier = "Apache-2.0"),
    ),
    tags(
        (name = "api", description = "The node's status and this document"),
        (name = "brc20", description = "BRC-20 and BRC2.0"),
        (name = "bitmap", description = "Bitmap"),
        (name = "sns", description = "SNS names and namespaces"),
        (name = "runes", description = "Runes"),
        (name = "stream", description = "The push feed of each block's events"),
    ),
    components(schemas(ErrorBody))
)]
pub struct ApiDoc;

/// A schema written as JSON Schema, for a shape the derive cannot state.
/// utoipa's `Schema` silently drops keywords it has no field for (`const`
/// among them, so a single value is an `enum` of one), which the round trip
/// refuses.
pub fn json_schema(value: serde_json::Value) -> RefOr<Schema> {
    let schema: RefOr<Schema> = serde_json::from_value(value.clone()).expect("a valid schema");
    assert_eq!(
        serde_json::to_value(&schema).expect("a schema serializes"),
        value,
        "utoipa dropped a keyword"
    );
    schema
}

/// `T`'s schema with a `default`, for a query parameter of a schema type
/// (`#[param(schema_with = ...)]`): the `default` attribute on such a
/// parameter wraps the schema in an `allOf` with an object type, which no
/// value matches.
pub fn with_default<T: PartialSchema>(default: &str) -> RefOr<Schema> {
    let mut schema = serde_json::to_value(T::schema()).expect("a schema serializes");
    schema["default"] = default.into();
    json_schema(schema)
}

/// JSON null.
pub struct Null;

impl PartialSchema for Null {
    fn schema() -> RefOr<Schema> {
        json_schema(json!({"type": "null"}))
    }
}

impl ToSchema for Null {}

/// A route's answer: `error` null and the `result`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct Answer<T: ToSchema> {
    error: Null,
    result: T,
}

/// A refusal or failure: `{"error":"<reason>","result":null}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct ErrorBody {
    /// Why the request failed.
    error: String,
    result: Null,
}

/// The latest block height a module's database holds: null before its
/// first block. The brc20 and runes routes give -1 when the height query
/// fails.
pub struct DbHeight;

impl PartialSchema for DbHeight {
    fn schema() -> RefOr<Schema> {
        json_schema(json!({
            "type": ["integer", "null"],
            "description": "The latest block height the module's database holds: null before its first block. The brc20 and runes routes give -1 when the height query fails.",
        }))
    }
}

impl ToSchema for DbHeight {}

/// A Postgres numeric as text, as node-pg reads numeric: a decimal integer
/// for amounts.
pub struct NumericText;

impl PartialSchema for NumericText {
    fn schema() -> RefOr<Schema> {
        json_schema(json!({
            "type": "string",
            "description": "A Postgres numeric as its text, as node-pg reads numeric: a decimal integer for amounts.",
            "examples": ["1000000000000000000"],
        }))
    }
}

impl ToSchema for NumericText {}

/// A query parameter as the route echoes it: the string the request gave,
/// or an array or object of strings for a repeated or bracketed parameter
/// (`a[]=1`, `a[b]=1`). The schema leaves out the deeper forms qs builds
/// from nested brackets (`a[b][c]=1`), which code generators handle poorly.
pub struct QueryEcho;

impl PartialSchema for QueryEcho {
    fn schema() -> RefOr<Schema> {
        json_schema(json!({
            "anyOf": [
                {"type": "string"},
                {"type": "array", "items": {"type": "string"}},
                {"type": "object", "additionalProperties": {"type": "string"}},
            ],
            "description": "The query parameter as the request gave it: a string, or an array or object of strings for a repeated or bracketed parameter (`a[]=1`, `a[b]=1`). Nested brackets (`a[b][c]=1`) nest further.",
        }))
    }
}

impl ToSchema for QueryEcho {}

/// The page of a list paged by offset.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct PageParams {
    /// The rows to skip.
    #[param(minimum = 0, maximum = 1000000000, default = 0)]
    offset: Option<i64>,
    /// The most rows to list.
    #[param(minimum = 1, maximum = 500, default = 100)]
    limit: Option<i64>,
}

/// The page size of a list paged by key.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct LimitParam {
    /// The most rows to list.
    #[param(minimum = 1, maximum = 500, default = 100)]
    limit: Option<i64>,
}

/// The `ticker` of a Node API route, which lowercases it unchecked.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct ParityTicker {
    /// The ticker, matched in lowercase. Left out, the route answers 500.
    ticker: String,
}

/// The `block_height` of a Node API hash route.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct HashBlockHeight {
    /// The block height, an integer. Text Postgres cannot read as an int4,
    /// a height without a hash row, or the parameter left out answers 500.
    block_height: String,
}

/// The `block_height` of a Node API route that lists a block's rows.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct ListBlockHeight {
    /// The block height, an integer. Text Postgres cannot read as an int4
    /// answers 500. Left out, the list is empty.
    block_height: Option<String>,
}

/// The document as `/v1/openapi.json` serves it, set once the router is
/// built.
pub type SpecJson = Arc<OnceLock<String>>;

/// This OpenAPI document.
///
/// It lists the routes of the modules this node serves (OPI_MODULES).
#[utoipa::path(
    get,
    path = "/v1/openapi.json",
    tag = "api",
    responses((status = 200, description = "The OpenAPI 3.1 document", body = Object)),
)]
pub async fn openapi_json(State(spec): State<SpecJson>) -> Response {
    let body = spec.get().cloned().unwrap_or_default();
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(reply::JSON));
    response
}

/// Gives each operation its id and the answers every operation shares: the
/// handler failure's 500 (an operation that states its own 500 keeps it)
/// and the rate limiter's 429.
pub fn finish(api: &mut openapi::OpenApi) {
    let internal = ResponseBuilder::new()
        .description(
            "The handler or its query failed: `{\"error\":\"internal error\",\"result\":null}`",
        )
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(ErrorBody::schema_ref()))
                .build(),
        )
        .build();
    let header = |description: &str| {
        HeaderBuilder::new()
            .schema(Some(json_schema(json!({"type": "string"}))))
            .description(Some(description))
            .build()
    };
    let limited = ResponseBuilder::new()
        .description("RATE_LIMIT_ENABLE is `true` and the client has used up its RATE_LIMIT_MAX requests of the window")
        .header(
            "RateLimit-Policy",
            header("`<RATE_LIMIT_MAX>;w=<window seconds>`"),
        )
        .header("RateLimit-Limit", header("RATE_LIMIT_MAX"))
        .header(
            "RateLimit-Remaining",
            header("The requests left in the window: 0"),
        )
        .header(
            "RateLimit-Reset",
            header("The seconds until the window ends; absent at 0"),
        )
        .header(
            "Retry-After",
            header("The seconds until the window ends"),
        )
        .content(
            "text/html",
            ContentBuilder::new()
                .schema(Some(json_schema(json!({
                    "type": "string",
                    "enum": [crate::rate_limit::MESSAGE],
                }))))
                .build(),
        )
        .build();
    for (path, method, operation) in operations_mut(api) {
        operation.operation_id = Some(operation_id(path, &method));
        let responses = &mut operation.responses.responses;
        responses
            .entry("500".into())
            .or_insert_with(|| internal.clone().into());
        responses
            .entry("429".into())
            .or_insert_with(|| limited.clone().into());
    }
}

/// An operation's id, unique in the document: its path after `/v1/` with
/// `_` for each `/` and `.` and without braces, after the method for a
/// method other than GET (`/v1/brc20/explorer/balances/{evm_address}` is
/// `brc20_explorer_balances_evm_address`). Handlers of different modules
/// share names, so the handler name is not unique.
fn operation_id(path: &str, method: &HttpMethod) -> String {
    let name = path
        .trim_start_matches("/v1/")
        .replace(['/', '.'], "_")
        .replace(['{', '}'], "");
    match method {
        HttpMethod::Get => name,
        method => {
            let method = serde_json::to_value(method).expect("a method serializes");
            format!("{}_{name}", method.as_str().unwrap_or_default())
        }
    }
}

/// Each operation of `api` with its path and method.
pub fn operations_mut(
    api: &mut openapi::OpenApi,
) -> impl Iterator<Item = (&str, HttpMethod, &mut Operation)> {
    api.paths.paths.iter_mut().flat_map(|(path, item)| {
        [
            (HttpMethod::Get, item.get.as_mut()),
            (HttpMethod::Put, item.put.as_mut()),
            (HttpMethod::Post, item.post.as_mut()),
            (HttpMethod::Delete, item.delete.as_mut()),
            (HttpMethod::Options, item.options.as_mut()),
            (HttpMethod::Head, item.head.as_mut()),
            (HttpMethod::Patch, item.patch.as_mut()),
            (HttpMethod::Trace, item.trace.as_mut()),
        ]
        .into_iter()
        .filter_map(|(method, operation)| {
            operation.map(|operation| (path.as_str(), method, operation))
        })
    })
}

/// The `$ref` of a schema type registered in the document.
trait SchemaRef {
    fn schema_ref() -> RefOr<Schema>;
}

impl<T: ToSchema> SchemaRef for T {
    fn schema_ref() -> RefOr<Schema> {
        RefOr::Ref(openapi::Ref::from_schema_name(T::name()))
    }
}
