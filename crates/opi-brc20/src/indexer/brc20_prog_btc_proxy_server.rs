use axum::{
    Router,
    extract::{OriginalUri, Request},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Client;
use std::{
    net::{SocketAddr, TcpListener},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

use crate::{
    config::{BitcoinRpcAuth, Brc20IndexerConfig},
    database::{BitcoinRpcBuffer, BitcoinRpcCache, connect_pool},
};

/// How long a proxy start retries an address that is still in use.
const BIND_RETRY: Duration = Duration::from_secs(5);
/// The pause between two bind attempts.
const BIND_RETRY_PAUSE: Duration = Duration::from_millis(100);
/// How long a forwarded request may take before the proxy answers 502.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Starts the Bitcoin RPC proxy the engine calls for Bitcoin data and returns
/// the address it listens on. It binds
/// `brc20_prog_bitcoin_rpc_proxy_server_addr` before returning, retrying an
/// address still in use for up to 5 seconds, and serves on `runtime` until
/// that runtime stops. `runtime` must not be the one that awaits engine
/// calls, since those calls block on the proxy.
///
/// The proxy forwards to bitcoind with `bitcoin_rpc_auth`, re-reading a
/// cookie file per request, and drops the caller's Authorization and Host
/// headers. Results are cached through a Postgres pool the proxy opens on
/// `runtime`, and through `buffer`, which holds the results of the current
/// block.
pub fn start_bitcoin_proxy_server(
    config: &Brc20IndexerConfig,
    buffer: BitcoinRpcBuffer,
    runtime: &Handle,
) -> anyhow::Result<SocketAddr> {
    start_with_retry(config, buffer, runtime, BIND_RETRY)
}

fn start_with_retry(
    config: &Brc20IndexerConfig,
    buffer: BitcoinRpcBuffer,
    runtime: &Handle,
    retry: Duration,
) -> anyhow::Result<SocketAddr> {
    let configured = &config.brc20_prog_bitcoin_rpc_proxy_server_addr;
    let addr: SocketAddr = configured
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid Bitcoin RPC proxy address {configured}: {e}"))?;
    let listener = bind(addr, retry)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    // The pool, the client and the listener belong to `runtime`.
    let _context = runtime.enter();
    let server = axum_server::from_tcp(listener)?;
    let upstream = Arc::new(Upstream {
        cache: BitcoinRpcCache::new(
            connect_pool(config, 2),
            buffer,
            config.bitcoin_rpc_cache_enabled,
            config.first_inscription_height,
        ),
        client: Client::builder().timeout(UPSTREAM_TIMEOUT).build()?,
        bitcoin_rpc_url: config.bitcoin_rpc_url.clone(),
        auth: config.bitcoin_rpc_auth.clone(),
        light_client_mode: config.light_client_mode,
        network_type: config.network_type_string.clone(),
    });
    let app = Router::new().route(
        "/",
        any(move |method, uri, req| proxy(method, uri, req, upstream.clone())),
    );
    runtime.spawn(async move {
        if let Err(e) = server.serve(app.into_make_service()).await {
            tracing::error!("the Bitcoin RPC proxy stopped: {e}");
        }
    });
    tracing::info!("Bitcoin RPC proxy listening on {addr}");
    Ok(addr)
}

/// Binds `addr`, retrying for `retry` while the address is in use.
fn bind(addr: SocketAddr, retry: Duration) -> anyhow::Result<TcpListener> {
    let deadline = Instant::now() + retry;
    loop {
        match TcpListener::bind(addr) {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                std::thread::sleep(BIND_RETRY_PAUSE);
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "the Bitcoin RPC proxy cannot listen on {addr}: {e}"
                ));
            }
        }
    }
}

/// What a request reads and forwards through. The pool and the client belong
/// to the runtime that serves the proxy.
struct Upstream {
    cache: BitcoinRpcCache,
    client: Client,
    bitcoin_rpc_url: String,
    /// Resolved on every forwarded request.
    auth: BitcoinRpcAuth,
    light_client_mode: bool,
    network_type: String,
}

async fn proxy(
    method: Method,
    OriginalUri(uri): OriginalUri,
    req: Request,
    upstream: Arc<Upstream>,
) -> Response {
    if method != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let cache = &upstream.cache;

    let headers = req.headers().clone();
    let body = req.into_body();

    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    match cache.get(&body).await {
        Ok(Some(response)) => {
            let mut response = Response::new(response.into());
            *response.status_mut() = StatusCode::OK;
            return response;
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!("Failed to get cached Bitcoin RPC request: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if upstream.light_client_mode {
        // Return dummy data for getblockchaininfo and getnetworkinfo methods in light client
        let (request_method, id) = get_request_method_and_id(&body);
        let Some(request_method) = request_method else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        if request_method == "getblockchaininfo" {
            return generate_get_blockchain_info_response(id, upstream.network_type.clone());
        }

        if request_method == "getnetworkinfo" {
            return generate_get_network_info_response(id);
        }
    }

    // Forward if not cached
    let target_uri = format!(
        "{}{}",
        upstream.bitcoin_rpc_url,
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
    );

    // The proxy presents its own credentials, never the caller's.
    let mut headers = headers;
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::HOST);
    let request = upstream
        .client
        .post(&target_uri)
        .body(body.clone())
        .headers(headers);
    let request = match upstream.auth.credentials().await {
        Ok(Some((user, pass))) => request.basic_auth(user, Some(pass)),
        Ok(None) => request,
        Err(e) => {
            tracing::error!("the Bitcoin RPC proxy cannot authenticate to bitcoind: {e:#}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let forwarded = request.send().await;

    let resp = match forwarded {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("Bitcoin RPC proxy request to bitcoind failed: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = resp.status();
    let headers = resp.headers().clone();
    let response_bytes = match resp.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("Bitcoin RPC proxy failed to read bitcoind's response: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    if cache.cache(&body, &response_bytes).await.is_err() {
        tracing::error!("Failed to cache Bitcoin RPC request");
        return StatusCode::BAD_GATEWAY.into_response();
    }

    let mut response = Response::new(response_bytes.into());
    *response.status_mut() = status;
    for (key, value) in headers.iter() {
        response.headers_mut().append(key, value.clone());
    }
    response
}

fn get_request_method_and_id(request: &[u8]) -> (Option<String>, Option<serde_json::Value>) {
    let Some(request) = serde_json::from_slice::<serde_json::Value>(request).ok() else {
        return (None, None);
    };
    let method = request.get("method").cloned();
    let method = method.and_then(|m| m.as_str().map(|s| s.to_string()));
    let id = request.get("id").cloned();
    (method, id)
}

fn generate_get_blockchain_info_response(
    id: Option<serde_json::Value>,
    network_type: String,
) -> Response {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "chain": match network_type.as_str() {
                "mainnet" => "main",
                "testnet" => "test",
                _ => network_type.as_str(),
            },
            "blocks":0,
            "headers":0,
            "bestblockhash":"0000000000000000000000000000000000000000000000000000000000000000",
            "bits":"00000000",
            "target":"0000000000000000000000000000000000000000000000000000000000000000",
            "difficulty":0,
            "time":0,
            "mediantime":0,
            "verificationprogress":1,
            "initialblockdownload":false,
            "chainwork":"0000000000000000000000000000000000000000000000000000000000000000",
            "size_on_disk":0,
            "pruned":false,
            "error":null,
            "warnings": ""
        }
    });
    Response::new(serde_json::to_vec(&response).unwrap().into())
}

fn generate_get_network_info_response(id: Option<serde_json::Value>) -> Response {
    let response = serde_json::json!({
        "error": null,
        "id": id,
        "result": {
            "version": 190001, // Bitcoin Core version that sends raw json
        }
    });
    Response::new(serde_json::to_vec(&response).unwrap().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_runtime::ServiceRuntime;
    use std::sync::Mutex;

    /// A bitcoind stand-in that records the Authorization header of every
    /// request (`None` when absent) and answers `{"result":0}`. Returns its URL.
    fn spawn_recording_bitcoind(seen: Arc<Mutex<Vec<Option<String>>>>) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut length, mut authorization) = (0usize, None);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap_or((line, ""));
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap_or(0),
                        "authorization" => authorization = Some(value.trim().to_string()),
                        _ => {}
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).ok();
                seen.lock().unwrap().push(authorization);
                let response = r#"{"result":0,"error":null,"id":1}"#;
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .ok();
            }
        });
        url
    }

    /// A proxy in front of a recording bitcoind, authenticating with `auth`.
    fn proxy_to_recorder(
        runtime: &ServiceRuntime,
        auth: BitcoinRpcAuth,
    ) -> (SocketAddr, Arc<Mutex<Vec<Option<String>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let config = Brc20IndexerConfig {
            bitcoin_rpc_url: spawn_recording_bitcoind(seen.clone()),
            bitcoin_rpc_auth: auth,
            ..config("127.0.0.1:0")
        };
        (start(&config, runtime).unwrap(), seen)
    }

    #[test]
    fn the_proxy_sends_its_own_credentials_and_drops_the_callers() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let (proxy, seen) = proxy_to_recorder(
            &runtime,
            BitcoinRpcAuth::UserPass("rpcuser".into(), "rpcpass".into()),
        );
        let answer = post(proxy, "getblockcount", Some("Basic aW5jb21pbmc6eA=="));
        assert!(answer.contains(r#""result":0"#), "{answer}");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Some("Basic cnBjdXNlcjpycGNwYXNz".to_string())]
        );
    }

    #[test]
    fn the_cookie_is_read_on_every_request() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let cookie = std::env::temp_dir().join(format!(
            "opi-brc20-proxy-cookie-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&cookie, "__cookie__:first").unwrap();
        let (proxy, seen) = proxy_to_recorder(&runtime, BitcoinRpcAuth::Cookie(cookie.clone()));

        post(proxy, "getblockcount", None);
        // bitcoind rewrites the cookie when it restarts.
        std::fs::write(&cookie, "__cookie__:second\n").unwrap();
        post(proxy, "getblockcount", None);
        std::fs::remove_file(&cookie).unwrap();
        let unreadable = post(proxy, "getblockcount", None);

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                Some("Basic X19jb29raWVfXzpmaXJzdA==".to_string()),
                Some("Basic X19jb29raWVfXzpzZWNvbmQ=".to_string()),
            ]
        );
        assert!(unreadable.is_empty(), "a 502 has no body: {unreadable}");
    }

    #[test]
    fn no_credentials_send_no_authorization() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let (proxy, seen) = proxy_to_recorder(&runtime, BitcoinRpcAuth::None);
        post(proxy, "getblockcount", Some("Basic aW5jb21pbmc6eA=="));
        assert_eq!(*seen.lock().unwrap(), vec![None]);
    }

    fn config(addr: &str) -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            brc20_prog_bitcoin_rpc_proxy_server_addr: addr.to_string(),
            bitcoin_rpc_cache_enabled: false,
            light_client_mode: false,
            ..Default::default()
        }
    }

    /// The body of the proxy's answer to a JSON-RPC call of `method`, sent
    /// with `authorization` when given.
    fn post(addr: SocketAddr, method: &str, authorization: Option<&str>) -> String {
        use std::io::{Read, Write};
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":[]}}"#);
        let authorization = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: {addr}\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or(response)
    }

    /// Starts a proxy on an empty buffer.
    fn start(config: &Brc20IndexerConfig, runtime: &ServiceRuntime) -> anyhow::Result<SocketAddr> {
        start_bitcoin_proxy_server(config, BitcoinRpcBuffer::default(), &runtime.handle())
    }

    #[test]
    fn a_taken_address_is_an_error_for_the_caller() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let started = Instant::now();
        let err = start_with_retry(
            &config(&addr),
            BitcoinRpcBuffer::default(),
            &runtime.handle(),
            Duration::from_millis(300),
        )
        .expect_err("the address is in use");
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(
            err.to_string()
                .starts_with(&format!("the Bitcoin RPC proxy cannot listen on {addr}: ")),
            "{err}"
        );
    }

    #[test]
    fn an_address_freed_during_the_retry_is_bound() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(taken);
        });
        let bound = start(&config(&addr.to_string()), &runtime)
            .expect("the address frees up within the retry");
        assert_eq!(bound, addr);
    }

    #[test]
    fn an_unparsable_address_keeps_the_parse_error() {
        let runtime = ServiceRuntime::new("proxy-test").unwrap();
        let err = start(&config("localhost"), &runtime).expect_err("not a socket address");
        assert_eq!(
            err.to_string(),
            "invalid Bitcoin RPC proxy address localhost: invalid socket address syntax"
        );
    }
}
