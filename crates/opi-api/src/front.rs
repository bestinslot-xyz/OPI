//! The middleware in front of every route, in the app's order: CORS
//! (`origin: '*'`, a preflight answers 200 with no body), then the rate
//! limiter keyed by `request.ip`, then routing as Express matches paths
//! (case-insensitive, one optional trailing slash), with any panic in a
//! handler answered like a thrown error.

use {
    crate::{
        client_ip::client_ip,
        extract::{RequestIp, strip_trailing_slash},
        rate_limit::{MESSAGE, RateLimiter},
        reply::{Reply, internal_error},
    },
    axum::{
        Router,
        extract::{ConnectInfo, OriginalUri, Request, State},
        http::{HeaderMap, HeaderName, HeaderValue, Method, Uri, header, uri::PathAndQuery},
        middleware::{self, Next},
        response::{IntoResponse, Response},
    },
    futures::FutureExt,
    std::{
        net::SocketAddr,
        panic::AssertUnwindSafe,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    },
};

/// The front door's settings.
pub struct FrontDoor {
    /// API_TRUSTED_PROXY_CNT as Express's `trust proxy` hop count.
    pub trusted_hops: usize,
    /// The rate limiter, when RATE_LIMIT_ENABLE is `true`.
    pub limiter: Option<Arc<RateLimiter>>,
}

/// Puts the front door before `inner`'s routes.
pub fn front_door(inner: Router, front: FrontDoor) -> Router {
    Router::new()
        .fallback_service(inner)
        .layer(middleware::from_fn_with_state(Arc::new(front), handle))
}

/// cors 2.8.5's preflight answer with `optionsSuccessStatus: 200`.
fn preflight(request_headers: &HeaderMap) -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,HEAD,PUT,PATCH,POST,DELETE"),
    );
    headers.insert(
        header::VARY,
        HeaderValue::from_static("Access-Control-Request-Headers"),
    );
    // the requested headers are reflected when present and non-empty
    if let Some(requested) = request_headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .filter(|v| !v.is_empty())
    {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// node's view of a header: the values of a repeated header joined with
/// ", ", each byte read as Latin-1.
fn latin1_header(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    let values: Vec<String> = headers
        .get_all(name)
        .iter()
        .map(|v| v.as_bytes().iter().map(|&b| char::from(b)).collect())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

/// The path Express's route regexes see it as: lower case, without one
/// trailing slash. The query string stays as it is.
fn routing_uri(uri: &Uri) -> Option<Uri> {
    let path = strip_trailing_slash(uri.path()).to_ascii_lowercase();
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::try_from(path_and_query).ok()?);
    Uri::from_parts(parts).ok()
}

fn insert_headers(response: &mut Response, rate_headers: &[(&'static str, String)]) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    for (name, value) in rate_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
}

async fn handle(State(front): State<Arc<FrontDoor>>, mut request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS {
        return preflight(request.headers());
    }

    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| {
            let forwarded = latin1_header(
                request.headers(),
                &HeaderName::from_static("x-forwarded-for"),
            );
            client_ip(
                &peer.ip().to_string(),
                forwarded.as_deref(),
                front.trusted_hops,
            )
        });
    if let Some(ip) = &ip {
        request.extensions_mut().insert(RequestIp(ip.clone()));
    }

    let mut rate_headers = Vec::new();
    if let Some(limiter) = &front.limiter {
        // express-rate-limit keys on request.ip, undefined without a peer
        let hit = limiter.hit(ip.as_deref().unwrap_or("undefined"), now_ms());
        rate_headers = hit.headers;
        if hit.blocked {
            let mut response = Reply::send_str(MESSAGE).status(429).into_response();
            insert_headers(&mut response, &rate_headers);
            return response;
        }
    }

    let original = request.uri().clone();
    if request.extensions().get::<OriginalUri>().is_none() {
        request
            .extensions_mut()
            .insert(OriginalUri(original.clone()));
    }
    if let Some(uri) = routing_uri(&original) {
        *request.uri_mut() = uri;
    }

    let mut response = match AssertUnwindSafe(next.run(request)).catch_unwind().await {
        Ok(response) => response,
        Err(_) => {
            tracing::error!("a handler panicked");
            internal_error()
        }
    };
    insert_headers(&mut response, &rate_headers);
    response
}
