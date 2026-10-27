//! The shared surface over HTTP. Expected statuses, headers and bodies are
//! the Node API's wire contract.

use {
    axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Method, Request, StatusCode},
        routing::get,
    },
    opi_api::{
        FrontDoor,
        config::{Config, Module, load_config},
        db::Db,
        extract::{ClientIp, ExpressParams, JsQuery},
        front_door,
        js::{JsValue, json_stringify},
        js_object,
        rate_limit::RateLimiter,
        reply::{ApiError, Reply},
        router,
    },
    std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration},
    tower::ServiceExt,
};

const NOT_ENABLED_SNS: &str = r#"{"error":"sns is not enabled on this node"}"#;
const INTERNAL_ERROR: &str = r#"{"error":"internal error","result":null}"#;

fn config(pairs: &[(&str, &str)]) -> Config {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    load_config(|key| map.get(key).cloned()).unwrap()
}

fn all_modules() -> Config {
    config(&[
        ("OPI_MODULES", "brc20,bitmap,sns,runes"),
        ("DB_DATABASE", "b"),
        ("BITMAP_DB_DATABASE", "m"),
        ("SNS_DB_DATABASE", "s"),
        ("RUNES_DB_DATABASE", "r"),
    ])
}

fn height(value: JsValue) -> Db {
    Db::fake(move |_| Ok(value.clone()))
}

fn failing() -> Db {
    Db::fake(|_| anyhow::bail!("database \"s\" does not exist"))
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }
}

async fn send_from(
    app: &Router,
    method: Method,
    uri: &str,
    peer: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::empty()).unwrap();
    let peer: SocketAddr = peer.parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

async fn send(app: &Router, method: Method, uri: &str) -> Answer {
    send_from(app, method, uri, "127.0.0.1:50000", &[]).await
}

fn express_404(method: &str, path: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot {method} {path}</pre>\n</body>\n</html>\n"
    )
}

#[tokio::test]
async fn a_disabled_module_answers_404_under_its_prefix() {
    let app = router(
        &config(&[("DB_DATABASE", "b")]),
        vec![(Module::Brc20, height(7.into()))],
    );
    for (method, path, module) in [
        (Method::GET, "/v1/bitmap/block_height", "bitmap"),
        (Method::GET, "/v1/sns/ip", "sns"),
        (Method::GET, "/v1/runes/event", "runes"),
        (Method::GET, "/v1/runes", "runes"),
        (Method::GET, "/v1/runes/", "runes"),
        (Method::GET, "/v1/sns//deep/path?x=1", "sns"),
        // app.use('/v1/sns') matches `/v1/sns/` followed by `/`
        (Method::GET, "/v1/sns//", "sns"),
        (Method::POST, "/V1/SNS", "sns"),
        (Method::DELETE, "/v1/Bitmap/x", "bitmap"),
    ] {
        let answer = send(&app, method.clone(), path).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(
            answer.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            answer.body,
            format!(r#"{{"error":"{module} is not enabled on this node"}}"#)
        );
    }
}

#[tokio::test]
async fn a_path_outside_every_route_is_expresss_404_page() {
    let app = router(&all_modules(), vec![]);
    for (method, path, shown) in [
        (Method::GET, "/v1/runesx", "/v1/runesx"),
        (Method::GET, "/a%zz%41b%7B?q=1", "/a%25zz%41b%7B"),
        (Method::GET, "/v1/status//", "/v1/status//"),
        (Method::POST, "/v1/status", "/v1/status"),
        (Method::PUT, "/", "/"),
    ] {
        let answer = send(&app, method.clone(), path).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(
            answer.header("content-type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            answer.header("content-security-policy"),
            Some("default-src 'none'")
        );
        assert_eq!(answer.header("x-content-type-options"), Some("nosniff"));
        // Express never names the allowed methods
        assert_eq!(answer.header("allow"), None, "{method} {path}");
        assert_eq!(answer.body, express_404(method.as_str(), shown));
    }
}

#[tokio::test]
async fn an_enabled_module_keeps_its_prefix_for_its_own_routes() {
    // A path under an enabled module that none of its routes takes falls
    // through to the 404 page, not the JSON 404.
    let app = router(&all_modules(), vec![]);
    let answer = send(&app, Method::GET, "/v1/sns/no_such_route").await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.body, express_404("GET", "/v1/sns/no_such_route"));
    assert_ne!(answer.body, NOT_ENABLED_SNS);
}

#[tokio::test]
async fn status_lists_each_enabled_module_with_its_latest_height() {
    let app = router(
        &config(&[
            ("OPI_MODULES", "brc20,bitmap,sns"),
            ("DB_DATABASE", "b"),
            ("BITMAP_DB_DATABASE", "m"),
            ("SNS_DB_DATABASE", "s"),
        ]),
        vec![
            (
                Module::Brc20,
                Db::fake(|module| {
                    assert_eq!(module, Module::Brc20);
                    Ok(100.into())
                }),
            ),
            (Module::Bitmap, height(JsValue::Null)),
            (Module::Sns, failing()),
        ],
    );
    let answer = send(&app, Method::GET, "/v1/status").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    // Keys follow completion order, as Promise.all's callbacks assign them.
    let mut body = answer.body.clone();
    for (module, value) in [
        ("brc20", r#"{"block_height":100}"#),
        ("bitmap", r#"{"block_height":null}"#),
        ("sns", r#"{"error":"unavailable"}"#),
    ] {
        let entry = format!(r#""{module}":{value}"#);
        assert!(body.contains(&entry), "{module} in {}", answer.body);
        body = body.replace(&entry, "");
    }
    assert_eq!(body, r#"{"error":null,"result":{"modules":{,,}}}"#);
}

#[tokio::test]
async fn status_reports_an_unreachable_database_as_unavailable() {
    let options = "postgres://u@127.0.0.1:1/db".parse().unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy_with(options);
    let app = router(
        &config(&[("OPI_MODULES", "runes"), ("RUNES_DB_DATABASE", "r")]),
        vec![(Module::Runes, Db::from_pool(pool))],
    );
    let answer = send(&app, Method::GET, "/v1/status").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.body,
        r#"{"error":null,"result":{"modules":{"runes":{"error":"unavailable"}}}}"#
    );
}

#[tokio::test]
async fn routes_match_case_insensitively_with_one_optional_trailing_slash() {
    let app = router(
        &config(&[("DB_DATABASE", "b")]),
        vec![(Module::Brc20, height(5.into()))],
    );
    let expected = r#"{"error":null,"result":{"modules":{"brc20":{"block_height":5}}}}"#;
    for path in ["/v1/status", "/V1/STATUS/", "/v1/Status?x=1"] {
        let answer = send(&app, Method::GET, path).await;
        assert_eq!(answer.status, StatusCode::OK, "{path}");
        assert_eq!(answer.body, expected, "{path}");
    }
    let head = send(&app, Method::HEAD, "/v1/status").await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.body, "");
}

#[tokio::test]
async fn cors_allows_any_origin_and_answers_options_with_200() {
    let app = router(
        &config(&[("DB_DATABASE", "b")]),
        vec![(Module::Brc20, height(5.into()))],
    );
    let get = send_from(
        &app,
        Method::GET,
        "/v1/status",
        "127.0.0.1:1",
        &[("Origin", "http://x")],
    )
    .await;
    assert_eq!(get.header("access-control-allow-origin"), Some("*"));
    assert_eq!(get.header("vary"), None);
    let not_found = send(&app, Method::GET, "/nope").await;
    assert_eq!(not_found.header("access-control-allow-origin"), Some("*"));

    for (path, request_headers, allow_headers) in [
        ("/v1/status", None, None),
        (
            "/nope",
            Some("x-a, content-type"),
            Some("x-a, content-type"),
        ),
    ] {
        let mut headers = vec![("Access-Control-Request-Method", "GET")];
        if let Some(value) = request_headers {
            headers.push(("Access-Control-Request-Headers", value));
        }
        let answer = send_from(&app, Method::OPTIONS, path, "127.0.0.1:1", &headers).await;
        assert_eq!(answer.status, StatusCode::OK, "{path}");
        assert_eq!(answer.body, "");
        assert_eq!(answer.header("content-length"), Some("0"));
        assert_eq!(answer.header("content-type"), None);
        assert_eq!(answer.header("access-control-allow-origin"), Some("*"));
        assert_eq!(
            answer.header("access-control-allow-methods"),
            Some("GET,HEAD,PUT,PATCH,POST,DELETE")
        );
        assert_eq!(
            answer.header("vary"),
            Some("Access-Control-Request-Headers")
        );
        assert_eq!(answer.header("access-control-allow-headers"), allow_headers);
    }

    // node joins a repeated Access-Control-Request-Headers with ", "
    let repeated = send_from(
        &app,
        Method::OPTIONS,
        "/v1/status",
        "127.0.0.1:1",
        &[
            ("Access-Control-Request-Headers", "x-a"),
            ("Access-Control-Request-Headers", "x-b"),
        ],
    )
    .await;
    assert_eq!(
        repeated.header("access-control-allow-headers"),
        Some("x-a, x-b")
    );
}

#[tokio::test]
async fn the_rate_limiter_counts_every_request_but_preflights() {
    let app = router(
        &config(&[
            ("DB_DATABASE", "b"),
            ("RATE_LIMIT_ENABLE", "true"),
            ("RATE_LIMIT_MAX", "2"),
            ("RATE_LIMIT_WINDOW_MS", "60000"),
        ]),
        vec![(Module::Brc20, height(5.into()))],
    );
    let first = send(&app, Method::GET, "/v1/status").await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.header("ratelimit-policy"), Some("2;w=60"));
    assert_eq!(first.header("ratelimit-limit"), Some("2"));
    assert_eq!(first.header("ratelimit-remaining"), Some("1"));
    assert_eq!(first.header("ratelimit-reset"), Some("60"));
    assert_eq!(first.header("retry-after"), None);

    let preflight = send(&app, Method::OPTIONS, "/v1/status").await;
    assert_eq!(preflight.status, StatusCode::OK);
    assert_eq!(preflight.header("ratelimit-limit"), None);

    let second = send(&app, Method::GET, "/nope").await;
    assert_eq!(second.status, StatusCode::NOT_FOUND);
    assert_eq!(second.header("ratelimit-remaining"), Some("0"));

    for _ in 0..2 {
        let limited = send(&app, Method::GET, "/v1/status").await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            limited.header("content-type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(limited.body, "Too many requests, please try again later.");
        assert_eq!(limited.header("access-control-allow-origin"), Some("*"));
        assert_eq!(limited.header("ratelimit-policy"), Some("2;w=60"));
        assert_eq!(limited.header("ratelimit-remaining"), Some("0"));
        assert_eq!(limited.header("ratelimit-reset"), Some("60"));
        assert_eq!(limited.header("retry-after"), Some("60"));
    }

    // another client address has its own window
    let other = send_from(&app, Method::GET, "/v1/status", "10.0.0.2:1", &[]).await;
    assert_eq!(other.status, StatusCode::OK);
}

#[tokio::test]
async fn the_rate_limiter_is_off_unless_enabled() {
    let app = router(
        &config(&[("DB_DATABASE", "b"), ("RATE_LIMIT_MAX", "1")]),
        vec![(Module::Brc20, height(5.into()))],
    );
    for _ in 0..3 {
        let answer = send(&app, Method::GET, "/v1/status").await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.header("ratelimit-limit"), None);
    }
}

/// A router with test routes behind the same front door the API uses.
fn test_app(trusted_hops: usize, limiter: Option<RateLimiter>) -> Router {
    async fn ip(ClientIp(ip): ClientIp) -> Reply {
        Reply::send_str(ip)
    }
    async fn query(JsQuery(query): JsQuery) -> Reply {
        Reply::send_json(query)
    }
    async fn param(ExpressParams(params): ExpressParams) -> Reply {
        Reply::send_json(params)
    }
    async fn fails() -> Result<Reply, ApiError> {
        let value = JsValue::Undefined;
        // request.query.ticker.toLowerCase() on a missing ticker
        Ok(Reply::send_str(value.to_lower_case()?))
    }
    async fn panics() -> Reply {
        panic!("handler bug")
    }
    let inner = Router::new()
        .route("/t/ip", get(ip))
        .route("/t/query", get(query))
        .route("/t/param/{evm_address}", get(param))
        .route("/t/fails", get(fails))
        .route("/t/panics", get(panics));
    front_door(
        inner,
        FrontDoor {
            trusted_hops,
            limiter: limiter.map(Arc::new),
        },
    )
}

#[tokio::test]
async fn the_client_ip_follows_trusted_proxy_hops() {
    let xff = [("X-Forwarded-For", " 1.1.1.1 , ,2.2.2.2,  3.3.3.3  ")];
    for (hops, expected) in [
        (0, "127.0.0.1"),
        (1, "3.3.3.3"),
        (2, "2.2.2.2"),
        (5, "1.1.1.1"),
    ] {
        let app = test_app(hops, None);
        let answer = send_from(&app, Method::GET, "/t/ip", "127.0.0.1:4000", &xff).await;
        assert_eq!(answer.body, expected, "trust proxy {hops}");
        assert_eq!(
            answer.header("content-type"),
            Some("text/html; charset=utf-8")
        );
        let without = send_from(&app, Method::GET, "/t/ip", "127.0.0.1:4000", &[]).await;
        assert_eq!(without.body, "127.0.0.1");
    }
    // an IPv4 client of a dual-stack listener, as node reports it
    let app = test_app(0, None);
    let mapped = send_from(&app, Method::GET, "/t/ip", "[::ffff:10.1.2.3]:9", &[]).await;
    assert_eq!(mapped.body, "::ffff:10.1.2.3");
}

#[tokio::test]
async fn the_rate_limiter_keys_on_the_client_ip() {
    let app = test_app(1, Some(RateLimiter::new(60_000.0, 1.0)));
    let from = |ip: &'static str| [("X-Forwarded-For", ip)];
    assert_eq!(
        send_from(&app, Method::GET, "/t/ip", "127.0.0.1:1", &from("1.1.1.1"))
            .await
            .status,
        200
    );
    assert_eq!(
        send_from(&app, Method::GET, "/t/ip", "127.0.0.1:1", &from("2.2.2.2"))
            .await
            .status,
        200
    );
    assert_eq!(
        send_from(&app, Method::GET, "/t/ip", "127.0.0.1:1", &from("1.1.1.1"))
            .await
            .status,
        429
    );
}

#[tokio::test]
async fn query_params_parse_like_express() {
    let app = test_app(0, None);
    let answer = send(&app, Method::GET, "/t/query?b=1&a=x&a=y&c[d]=&e").await;
    assert_eq!(
        answer.body,
        r#"{"b":"1","a":["x","y"],"c":{"d":""},"e":""}"#
    );
    // the query keeps its case when the path is matched case-insensitively
    let upper = send(&app, Method::GET, "/T/Query?Ticker=ORDI").await;
    assert_eq!(upper.body, r#"{"Ticker":"ORDI"}"#);
}

#[tokio::test]
async fn path_params_decode_like_express() {
    let app = test_app(0, None);
    let answer = send(&app, Method::GET, "/T/PARAM/0xAb%43+d/").await;
    assert_eq!(answer.body, r#"{"evm_address":"0xAbC+d"}"#);
    // decode_param's URIError goes to the error middleware: 500
    let bad = send(&app, Method::GET, "/t/param/%E0%A4%A").await;
    assert_eq!(bad.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(bad.body, INTERNAL_ERROR);
}

#[tokio::test]
async fn any_handler_failure_answers_500_and_serving_goes_on() {
    let app = test_app(0, None);
    for path in ["/t/fails", "/t/panics"] {
        let answer = send(&app, Method::GET, path).await;
        assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(
            answer.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(answer.body, INTERNAL_ERROR);
        assert_eq!(answer.header("access-control-allow-origin"), Some("*"));
    }
    assert_eq!(
        send(&app, Method::GET, "/t/ip").await.status,
        StatusCode::OK
    );
}

#[test]
fn json_stringify_is_reachable_for_route_code() {
    assert_eq!(
        json_stringify(&js_object! { "a" => 1 }.into()).as_deref(),
        Some(r#"{"a":1}"#)
    );
}
