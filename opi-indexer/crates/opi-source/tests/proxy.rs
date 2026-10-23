use {
    axum::{Router, body::Bytes, http::HeaderMap, routing::post},
    bitcoin::{Network, consensus::encode::serialize_hex, constants::genesis_block},
    opi_source::proxy::{BarrierRequest, GateProxy, ProxyConfig, UpstreamAuth},
    serde_json::{Value, json},
    std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    },
    tokio::sync::mpsc,
};

type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

/// A fake bitcoind that records every request and answers with `respond`.
struct Upstream {
    url: String,
    seen: Seen,
}

async fn upstream(respond: impl Fn(&Value) -> String + Send + Sync + 'static) -> Upstream {
    let seen = Seen::default();
    let respond = Arc::new(respond);
    let app = Router::new().route(
        "/",
        post({
            let seen = seen.clone();
            move |headers: HeaderMap, body: Bytes| {
                let seen = seen.clone();
                let respond = respond.clone();
                async move {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    seen.lock().unwrap().push((headers, body));
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        respond(&request),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Upstream { url, seen }
}

fn null_response(_: &Value) -> String {
    r#"{"result":null,"error":null,"id":1}"#.into()
}

async fn start(
    upstream: &Upstream,
    auth: UpstreamAuth,
) -> (GateProxy, mpsc::Receiver<BarrierRequest>) {
    let (tx, rx) = mpsc::channel(1);
    let proxy = GateProxy::start(
        ProxyConfig {
            upstream_url: upstream.url.clone(),
            auth,
            block_cache: 4,
        },
        tx,
    )
    .await
    .unwrap();
    (proxy, rx)
}

fn user_pass() -> UpstreamAuth {
    UpstreamAuth::UserPass("user".into(), "pass".into())
}

async fn post_body(proxy: &GateProxy, body: impl Into<reqwest::Body>) -> reqwest::Response {
    reqwest::Client::new()
        .post(proxy.url())
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn call(proxy: &GateProxy, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "1.0", "id": "ord", "method": method, "params": params});
    post_body(proxy, body.to_string())
        .await
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn getblockhash_past_chunk_end_returns_minus_8_without_forwarding() {
    let upstream = upstream(null_response).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;
    proxy.set_chunk_end(10);

    let response = post_body(
        &proxy,
        r#"{"jsonrpc":"1.0","id":"ord","method":"getblockhash","params":[11]}"#,
    )
    .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"result": null, "error": {"code": -8, "message": "Block height out of range"}, "id": "ord"})
    );
    assert!(upstream.seen.lock().unwrap().is_empty());

    call(&proxy, "getblockhash", json!([10])).await;
    assert_eq!(upstream.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn batch_request_is_forwarded_byte_for_byte() {
    const REPLY: &str =
        r#"[{"result":"aa","error":null,"id":0} , {"result":null,"error":null,"id":1}]"#;
    let upstream = upstream(|_| REPLY.into()).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;
    proxy.set_chunk_end(0);

    let body: &[u8] =
        br#"[ {"jsonrpc":"2.0","id":0,"method":"getrawtransaction","params":["ab",false]},
        {"jsonrpc":"2.0","id":1,"method":"getblockhash","params":[99]} ]"#;
    let response = post_body(&proxy, body).await;

    assert_eq!(response.text().await.unwrap(), REPLY);
    let seen = upstream.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.as_ref(), body);
}

#[tokio::test]
async fn upstream_auth_replaces_client_auth() {
    let upstream = upstream(null_response).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;

    reqwest::Client::new()
        .post(proxy.url())
        .basic_auth("foo", Some("bar"))
        .body(r#"{"jsonrpc":"1.0","id":1,"method":"getnetworkinfo","params":[]}"#)
        .send()
        .await
        .unwrap();

    let seen = upstream.seen.lock().unwrap();
    let auth: Vec<_> = seen[0].0.get_all("authorization").iter().collect();
    assert_eq!(auth, ["Basic dXNlcjpwYXNz"]);
}

#[tokio::test]
async fn cookie_auth_reads_cookie_file_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let cookie: PathBuf = tmp.path().join(".cookie");
    std::fs::write(&cookie, "__cookie__:abc").unwrap();
    let upstream = upstream(null_response).await;
    let (proxy, _barriers) = start(&upstream, UpstreamAuth::Cookie(cookie.clone())).await;

    call(&proxy, "getnetworkinfo", json!([])).await;
    std::fs::write(&cookie, "__cookie__:xyz").unwrap();
    call(&proxy, "getnetworkinfo", json!([])).await;

    let seen = upstream.seen.lock().unwrap();
    let auth: Vec<_> = seen
        .iter()
        .map(|(headers, _)| headers["authorization"].to_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        auth,
        ["Basic X19jb29raWVfXzphYmM=", "Basic X19jb29raWVfXzp4eXo="]
    );
}

const HASH: &str = "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206";

#[tokio::test]
async fn getblockhash_records_hash() {
    let upstream = upstream(|_| format!(r#"{{"result":"{HASH}","error":null,"id":"ord"}}"#)).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;
    proxy.set_chunk_end(10);

    let response = call(&proxy, "getblockhash", json!([5])).await;

    assert_eq!(response["result"], HASH);
    assert_eq!(proxy.hash_for(5), Some(HASH.parse().unwrap()));
    assert_eq!(proxy.hash_for(6), None);
}

#[tokio::test]
async fn close_ends_chunk_at_highest_served() {
    let upstream = upstream(|_| format!(r#"{{"result":"{HASH}","error":null,"id":"ord"}}"#)).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;
    proxy.set_chunk_end(10);
    call(&proxy, "getblockhash", json!([5])).await;

    proxy.close();

    assert_eq!(
        call(&proxy, "getblockhash", json!([5])).await["result"],
        HASH
    );
    assert_eq!(
        call(&proxy, "getblockhash", json!([6])).await["error"]["code"],
        -8
    );
}

#[tokio::test]
async fn getblock_is_cached() {
    let block = genesis_block(Network::Regtest);
    let hex = serialize_hex(&block);
    let upstream =
        upstream(move |_| format!(r#"{{"result":"{hex}","error":null,"id":"ord"}}"#)).await;
    let (proxy, _barriers) = start(&upstream, user_pass()).await;
    let hash = block.block_hash();
    assert_eq!(proxy.cached_block(&hash), None);

    call(&proxy, "getblock", json!([hash, 0])).await;

    assert_eq!(proxy.cached_block(&hash), Some(block));
}

#[tokio::test]
async fn getblockchaininfo_waits_for_barrier_reply() {
    let upstream = upstream(null_response).await;
    let (proxy, mut barriers) = start(&upstream, user_pass()).await;

    let mut request = tokio::spawn({
        let proxy = proxy.clone();
        async move { call(&proxy, "getblockchaininfo", json!([])).await }
    });

    let barrier = barriers.recv().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut request)
            .await
            .is_err()
    );
    assert!(upstream.seen.lock().unwrap().is_empty());

    barrier.send(()).unwrap();
    request.await.unwrap();
    assert_eq!(upstream.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn getblockchaininfo_fails_when_barrier_is_dropped() {
    let upstream = upstream(null_response).await;
    let (proxy, mut barriers) = start(&upstream, user_pass()).await;

    let request = tokio::spawn({
        let proxy = proxy.clone();
        async move { call(&proxy, "getblockchaininfo", json!([])).await }
    });
    drop(barriers.recv().await.unwrap());

    let response = request.await.unwrap();
    assert!(response["error"].is_object(), "{response}");
    assert!(upstream.seen.lock().unwrap().is_empty());
}
