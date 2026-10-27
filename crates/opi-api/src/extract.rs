//! What a route handler reads from the request, as Express exposes it:
//! `request.ip`, `request.query` and `request.params`.

use {
    crate::{
        js::{JsObject, decode_uri_component},
        qs::parse_query,
        reply::ApiError,
    },
    anyhow::anyhow,
    axum::{
        extract::{FromRequestParts, MatchedPath, OriginalUri},
        http::request::Parts,
    },
};

/// `request.ip`: the client address under the `trust proxy` hop count.
pub struct ClientIp(pub String);

/// The client address the front door worked out for the request.
#[derive(Clone)]
pub(crate) struct RequestIp(pub(crate) String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let ip = parts
            .extensions
            .get::<RequestIp>()
            .map(|ip| ip.0.clone())
            .ok_or_else(|| anyhow!("request.ip is undefined: the request has no peer address"))?;
        Ok(ClientIp(ip))
    }
}

/// `request.query`: the query string as qs parses it for Express.
pub struct JsQuery(pub JsObject);

impl<S: Send + Sync> FromRequestParts<S> for JsQuery {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        Ok(JsQuery(parse_query(parts.uri.query())))
    }
}

/// `request.params`: each `{name}` segment of the route, taken from the
/// path as the client sent it and decoded with decodeURIComponent. A
/// segment that does not decode fails the request with the 500 answer, as
/// Express's decode_param error reaches the app's error middleware.
pub struct ExpressParams(pub JsObject);

impl<S: Send + Sync> FromRequestParts<S> for ExpressParams {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let pattern = parts
            .extensions
            .get::<MatchedPath>()
            .ok_or_else(|| anyhow!("ExpressParams outside a matched route"))?
            .as_str()
            .to_string();
        let original = parts.extensions.get::<OriginalUri>().map_or_else(
            || parts.uri.path().to_string(),
            |uri| uri.0.path().to_string(),
        );
        let path = strip_trailing_slash(&original);
        let names: Vec<&str> = pattern.split('/').collect();
        let values: Vec<&str> = path.split('/').collect();
        if names.len() != values.len() {
            return Err(anyhow!("{path} does not have the segments of {pattern}").into());
        }
        let mut params = JsObject::new();
        for (name, value) in names.iter().zip(values) {
            if let Some(name) = name.strip_prefix('{').and_then(|n| n.strip_suffix('}')) {
                let decoded = decode_uri_component(value)
                    .ok_or_else(|| anyhow!("Failed to decode param '{value}'"))?;
                params.set(name, decoded);
            }
        }
        Ok(ExpressParams(params))
    }
}

/// Express routes (`strict: false`) accept one trailing slash.
pub(crate) fn strip_trailing_slash(path: &str) -> &str {
    if path.len() > 1 {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    }
}
