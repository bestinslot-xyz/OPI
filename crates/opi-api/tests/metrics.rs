//! The API's metrics listener: request metrics by route pattern, liveness,
//! and readiness from the module databases.

use {
    opi_api::{
        config::{Module, load_config},
        db::Db,
        js::JsValue,
        serve_listeners,
    },
    std::{collections::HashMap, net::SocketAddr, time::Duration},
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
    },
};

struct Api {
    api: SocketAddr,
    metrics: SocketAddr,
    /// Held so the server's shutdown future stays pending.
    _stop: oneshot::Sender<()>,
    done: JoinHandle<anyhow::Result<()>>,
}

impl Api {
    /// Stops the server without a shutdown, which would mark the whole
    /// test process shutting down; tests/metrics_shutdown.rs covers that.
    async fn stop(self) {
        self.done.abort();
    }
}

/// The API serving brc20 from `db`, with its metrics listener, each on an
/// ephemeral port.
async fn start(db: Db) -> Api {
    start_with(db, &[]).await
}

/// [`start`] with the settings `extra` adds.
async fn start_with(db: Db, extra: &[(&'static str, &'static str)]) -> Api {
    let env: HashMap<&str, &str> = [("OPI_MODULES", "brc20"), ("DB_DATABASE", "d")]
        .into_iter()
        .chain(extra.iter().copied())
        .collect();
    let config = load_config(|key| env.get(key).map(|v| v.to_string())).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (api, metrics) = (
        listener.local_addr().unwrap(),
        metrics_listener.local_addr().unwrap(),
    );
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(serve_listeners(
        config,
        vec![(Module::Brc20, db)],
        listener,
        Some(metrics_listener),
        async {
            // Only a sent stop shuts down; a dropped sender does not.
            if stopped.await.is_err() {
                std::future::pending::<()>().await;
            }
        },
    ));
    Api {
        api,
        metrics,
        _stop: stop,
        done,
    }
}

/// The status line and body of a GET of `path`.
async fn get(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut answer))
        .await
        .expect("an answer within 10 s")
        .unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[tokio::test]
async fn the_metrics_listener_counts_api_requests_by_route_pattern() {
    let api = start(Db::fake(|_| Ok(JsValue::Number(840_000.0)))).await;

    let (status, _) = get(api.api, "/v1/status").await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    let (status, _) = get(api.api, "/v1/brc20/explorer/balances/0xfeed").await;
    assert!(status.starts_with("HTTP/1.1 "), "{status}");

    let (status, body) = get(api.metrics, "/metrics").await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        body.contains(r#"opi_api_requests_total{module="api",route="/v1/status",status="200"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"module="brc20",route="/v1/brc20/explorer/balances/{evm_address}""#),
        "{body}"
    );
    assert!(!body.contains("0xfeed"), "{body}");
    assert!(
        body.contains(r#"opi_api_request_seconds_count{module="api",route="/v1/status"} 1"#),
        "{body}"
    );
    // The metrics listener's own requests are not API requests.
    assert!(!body.contains(r#"route="/metrics""#), "{body}");

    // The metrics listener does not serve the API, nor the API the metrics.
    let (status, _) = get(api.metrics, "/v1/status").await;
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    let (status, _) = get(api.api, "/metrics").await;
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
    api.stop().await;
}

#[tokio::test]
async fn rate_limit_rejections_count_under_the_module_of_their_path() {
    let api = start_with(
        Db::fake(|_| Ok(JsValue::Number(1.0))),
        &[("RATE_LIMIT_ENABLE", "true"), ("RATE_LIMIT_MAX", "1")],
    )
    .await;
    get(api.api, "/v1/brc20/block_height").await;
    let (status, _) = get(api.api, "/v1/brc20/block_height").await;
    assert_eq!(status, "HTTP/1.1 429 Too Many Requests");
    let (_, body) = get(api.metrics, "/metrics").await;
    assert!(
        body.contains(r#"opi_api_rate_limited_total{module="brc20"} 1"#),
        "{body}"
    );
    api.stop().await;
}

#[tokio::test]
async fn the_api_is_live_and_ready_when_its_databases_answer() {
    let api = start(Db::fake(|_| Ok(JsValue::Number(1.0)))).await;
    assert_eq!(get(api.metrics, "/health/live").await.0, "HTTP/1.1 200 OK");
    assert_eq!(get(api.metrics, "/health/ready").await.0, "HTTP/1.1 200 OK");
    api.stop().await;
}

#[tokio::test]
async fn the_api_is_not_ready_while_a_database_fails() {
    let api = start(Db::fake(|_| anyhow::bail!("connection refused"))).await;
    assert_eq!(get(api.metrics, "/health/live").await.0, "HTTP/1.1 200 OK");
    let (status, body) = get(api.metrics, "/health/ready").await;
    assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
    assert_eq!(body, "the brc20 database is not answering\n");
    api.stop().await;
}

#[tokio::test]
async fn unknown_paths_share_one_unmatched_series() {
    let api = start(Db::fake(|_| Ok(JsValue::Number(1.0)))).await;
    assert!(get(api.api, "/nope/first").await.0.contains("404"));
    assert!(get(api.api, "/elsewhere/second/x").await.0.contains("404"));
    let (_, body) = get(api.metrics, "/metrics").await;
    let unmatched: Vec<&str> = body
        .lines()
        .filter(|line| line.starts_with("opi_api_requests_total") && line.contains("unmatched"))
        .collect();
    assert_eq!(unmatched.len(), 1, "{body}");
    assert!(
        unmatched[0]
            .starts_with(r#"opi_api_requests_total{module="api",route="unmatched",status="404"} "#),
        "{body}"
    );
    assert!(
        !body.contains("nope") && !body.contains("elsewhere"),
        "{body}"
    );
    api.stop().await;
}

#[tokio::test]
async fn a_panicking_handler_counts_as_a_500() {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
        routing::get as route_get,
    };
    use tower::ServiceExt;

    async fn boom() -> &'static str {
        panic!("boom")
    }
    let app = opi_api::metrics::instrument(Router::new().route("/v1/sns/boom", route_get(boom)));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/sns/boom")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 500);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], br#"{"error":"internal error","result":null}"#);
    let text = opi_core::metrics::global().encode();
    assert!(
        text.contains(
            r#"opi_api_requests_total{module="sns",route="/v1/sns/boom",status="500"} 1"#
        ),
        "{text}"
    );
}
