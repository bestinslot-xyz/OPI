//! Gate proxy: a loopback JSON-RPC forwarder between ord and bitcoind.
//!
//! It forwards every request body unchanged and intercepts three calls:
//! `getblockhash` past the chunk end answers RPC error -8, which ends ord's
//! block stream; `getblock` (verbosity 0) results are cached for enrichment;
//! `getblockchaininfo` waits on a commit barrier until the stager has synced
//! every event received so far.

use {
    anyhow::{Context, Result, anyhow},
    axum::{
        Router,
        body::Bytes,
        extract::{DefaultBodyLimit, State},
        http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
        response::{IntoResponse, Response},
    },
    bitcoin::{Block, BlockHash, consensus::encode::deserialize_hex},
    serde_json::{Value, json},
    std::{
        collections::{BTreeMap, HashMap, VecDeque},
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicI64, Ordering},
        },
    },
    tokio::{
        sync::{mpsc, oneshot},
        task::JoinHandle,
    },
};

/// Credentials the proxy presents to bitcoind.
#[derive(Debug, Clone)]
pub enum UpstreamAuth {
    UserPass(String, String),
    /// bitcoind's cookie file, read on every request since bitcoind rewrites
    /// it on restart.
    Cookie(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub upstream_url: String,
    pub auth: UpstreamAuth,
    /// Number of raw blocks kept for [`GateProxy::cached_block`].
    pub block_cache: usize,
}

/// A commit barrier. The stager replies once every event received so far is
/// durably synced; dropping it unanswered fails ord's `getblockchaininfo`.
pub type BarrierRequest = oneshot::Sender<()>;

/// Handle to a running gate proxy. Clones share the proxy; the server task
/// stops when the last clone is dropped.
#[derive(Clone)]
pub struct GateProxy {
    url: String,
    state: Arc<ProxyState>,
    _server: Arc<ServerTask>,
}

struct ServerTask(JoinHandle<()>);

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct ProxyState {
    upstream_url: String,
    auth: UpstreamAuth,
    client: reqwest::Client,
    barriers: mpsc::Sender<BarrierRequest>,
    /// Highest height `getblockhash` answers; -1 answers none.
    chunk_end: AtomicI64,
    /// Highest height whose hash `getblockhash` returned; -1 when none.
    highest_served: AtomicI64,
    hashes: Mutex<BTreeMap<u32, BlockHash>>,
    blocks: Mutex<BlockCache>,
}

impl GateProxy {
    /// Binds `127.0.0.1:0` and serves until the last handle is dropped.
    /// `getblockhash` answers no height until [`GateProxy::set_chunk_end`].
    pub async fn start(
        cfg: ProxyConfig,
        barriers: mpsc::Sender<BarrierRequest>,
    ) -> Result<GateProxy> {
        let state = Arc::new(ProxyState {
            upstream_url: cfg.upstream_url,
            auth: cfg.auth,
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .context("building bitcoind client")?,
            barriers,
            chunk_end: AtomicI64::new(-1),
            highest_served: AtomicI64::new(-1),
            hashes: Mutex::default(),
            blocks: Mutex::new(BlockCache::new(cfg.block_cache)),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding gate proxy")?;
        let url = format!("http://{}", listener.local_addr()?);
        let app = Router::new()
            .fallback(handle)
            .layer(DefaultBodyLimit::disable())
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Ok(GateProxy {
            url,
            state,
            _server: Arc::new(ServerTask(server)),
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Makes `getblockhash` answer heights up to and including `height`.
    pub fn set_chunk_end(&self, height: u32) {
        self.state.chunk_end.store(height.into(), Ordering::SeqCst);
    }

    /// Ends the chunk at the highest height served so far. A `getblockhash`
    /// already past the gate may still serve one height beyond it, which is
    /// harmless.
    pub fn close(&self) {
        let served = self.state.highest_served.load(Ordering::SeqCst);
        self.state.chunk_end.store(served, Ordering::SeqCst);
    }

    /// The hash `getblockhash(height)` last returned.
    pub fn hash_for(&self, height: u32) -> Option<BlockHash> {
        self.state.hashes.lock().unwrap().get(&height).copied()
    }

    pub fn cached_block(&self, hash: &BlockHash) -> Option<Block> {
        self.state.blocks.lock().unwrap().get(hash)
    }
}

async fn handle(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = serde_json::from_slice::<Value>(&body)
        .ok()
        .filter(Value::is_object);
    let request = request.as_ref();
    let method = request.and_then(|r| r["method"].as_str());
    let id = request.map_or(Value::Null, |r| r["id"].clone());

    match method {
        Some("getblockhash") => {
            let height = request.and_then(|r| r["params"][0].as_u64());
            let chunk_end = state.chunk_end.load(Ordering::SeqCst);
            if height.is_some_and(|h| i64::try_from(h).unwrap_or(i64::MAX) > chunk_end) {
                return rpc_reply(
                    StatusCode::OK,
                    json!({
                        "result": null,
                        "error": {"code": -8, "message": "Block height out of range"},
                        "id": id,
                    }),
                );
            }
        }
        Some("getblockchaininfo") => {
            if let Err(err) = state.barrier().await {
                return rpc_reply(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({
                        "result": null,
                        "error": {"code": -32603, "message": format!("{err:#}")},
                        "id": id,
                    }),
                );
            }
        }
        _ => {}
    }

    let (status, content_type, reply) = match state.forward(&uri, &headers, body).await {
        Ok(forwarded) => forwarded,
        Err(err) => return (StatusCode::BAD_GATEWAY, format!("{err:#}")).into_response(),
    };

    if status.is_success()
        && let (Some(method), Some(request)) = (method, request)
    {
        state.observe(method, request, &reply);
    }

    let mut response = (status, reply).into_response();
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

fn rpc_reply(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

impl ProxyState {
    async fn barrier(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.barriers
            .send(tx)
            .await
            .map_err(|_| anyhow!("commit barrier failed: stager stopped"))?;
        rx.await
            .map_err(|_| anyhow!("commit barrier failed: stager did not sync"))
    }

    async fn credentials(&self) -> Result<(String, String)> {
        match &self.auth {
            UpstreamAuth::UserPass(user, pass) => Ok((user.clone(), pass.clone())),
            UpstreamAuth::Cookie(path) => {
                let cookie = tokio::fs::read_to_string(path)
                    .await
                    .with_context(|| format!("reading cookie file {}", path.display()))?;
                let (user, pass) = cookie
                    .trim_end()
                    .split_once(':')
                    .with_context(|| format!("malformed cookie file {}", path.display()))?;
                Ok((user.into(), pass.into()))
            }
        }
    }

    async fn forward(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<(StatusCode, Option<HeaderValue>, Bytes)> {
        let url = format!(
            "{}{}",
            self.upstream_url.trim_end_matches('/'),
            uri.path_and_query().map_or("/", |p| p.as_str())
        );
        let (user, pass) = self.credentials().await?;
        let mut request = self
            .client
            .post(url)
            .basic_auth(user, Some(pass))
            .body(body);
        if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let response = request.send().await.context("forwarding to bitcoind")?;
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        let reply = response
            .bytes()
            .await
            .context("reading bitcoind response")?;
        Ok((status, content_type, reply))
    }

    /// Records what a successful reply to an intercepted call reveals.
    fn observe(&self, method: &str, request: &Value, reply: &[u8]) {
        let params = &request["params"];
        match method {
            "getblockhash" => {
                let height = params[0].as_u64().and_then(|h| u32::try_from(h).ok());
                let hash = result_str(reply).and_then(|hash| hash.parse::<BlockHash>().ok());
                if let (Some(height), Some(hash)) = (height, hash) {
                    self.hashes.lock().unwrap().insert(height, hash);
                    self.highest_served
                        .fetch_max(height.into(), Ordering::SeqCst);
                }
            }
            "getblock" => {
                let raw = matches!(&params[1], Value::Number(n) if n.as_u64() == Some(0))
                    || params[1] == Value::Bool(false);
                if raw
                    && let Some(block) =
                        result_str(reply).and_then(|hex| deserialize_hex::<Block>(&hex).ok())
                {
                    self.blocks.lock().unwrap().insert(block);
                }
            }
            _ => {}
        }
    }
}

/// The string `result` of a JSON-RPC reply without an error.
fn result_str(reply: &[u8]) -> Option<String> {
    let reply = serde_json::from_slice::<Value>(reply).ok()?;
    if !reply["error"].is_null() {
        return None;
    }
    reply["result"].as_str().map(str::to_owned)
}

/// Least-recently-used cache of raw blocks.
struct BlockCache {
    capacity: usize,
    order: VecDeque<BlockHash>,
    blocks: HashMap<BlockHash, Block>,
}

impl BlockCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            blocks: HashMap::new(),
        }
    }

    fn touch(&mut self, hash: BlockHash) {
        self.order.retain(|h| *h != hash);
        self.order.push_back(hash);
    }

    fn insert(&mut self, block: Block) {
        if self.capacity == 0 {
            return;
        }
        let hash = block.block_hash();
        self.blocks.insert(hash, block);
        self.touch(hash);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.blocks.remove(&evicted);
            }
        }
    }

    fn get(&mut self, hash: &BlockHash) -> Option<Block> {
        let block = self.blocks.get(hash).cloned()?;
        self.touch(*hash);
        Some(block)
    }
}
