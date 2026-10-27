//! The embedded engine's source of Bitcoin data: brc20-prog's lookups,
//! answered from the Bitcoin RPC result cache and, in full mode, bitcoind.
//!
//! Cache keys and stored responses are bitcoincore_rpc 0.19's requests and
//! bitcoind's answers to them, so the cache table stays interchangeable with
//! what full nodes publish and light clients replay: the key is the request
//! object `{"method", "params", "jsonrpc": "2.0"}` without its `id`, with
//! trailing default parameters left out, and the stored response is
//! bitcoind's answer without its `id`. Answers are read as brc20-prog's
//! `BitcoinCoreRpc` reads them, whether they come from the cache or from
//! bitcoind.

use std::{
    error::Error,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::RecvTimeoutError,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::body::Bytes;
use bitcoin::{BlockHash, Network, Transaction, Txid, consensus::encode};
use bitcoincore_rpc::{
    json::{GetBlockHeaderResult, GetBlockchainInfoResult, GetRawTransactionResult},
    jsonrpc,
};
use brc20_prog::{BitcoinRpc, BitcoinRpcError};
use reqwest::{Client, header};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::{Pool, Postgres};
use tokio::sync::mpsc;

use crate::{
    config::{BitcoinRpcAuth, Brc20IndexerConfig},
    database::{BitcoinRpcBuffer, BitcoinRpcCache, connect_pool},
    fail_stop::BRC20_THREAD_NAME,
    service_runtime::ServiceRuntime,
};

/// How long one request to bitcoind may take.
const BITCOIND_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a lookup may take, cache reads and writes included.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a lookup waits for a connection from the source's Postgres pool.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// bitcoind's error code for an unknown transaction or block
/// (RPC_INVALID_ADDRESS_OR_KEY).
const NOT_FOUND: i32 = -5;

/// The buffer of uncommitted results that lookups read and write. An indexer
/// installs its database's buffer here; until then the slot holds an empty
/// buffer of its own.
#[derive(Clone, Default)]
pub struct BitcoinRpcBufferSlot(Arc<Mutex<BitcoinRpcBuffer>>);

impl BitcoinRpcBufferSlot {
    /// Makes `buffer` the one later lookups use.
    pub fn install(&self, buffer: BitcoinRpcBuffer) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = buffer;
    }

    /// The installed buffer.
    pub fn current(&self) -> BitcoinRpcBuffer {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// brc20-prog's Bitcoin data source, backed by the Bitcoin RPC result cache.
///
/// A lookup reads the installed buffer, then the
/// `brc20_bitcoin_rpc_result_cache` table's newest row, when
/// `BITCOIN_RPC_CACHE_ENABLED` is on. In full mode a stored "not yet" row is
/// skipped (see `serves_stored`) and a miss goes to bitcoind, authenticating
/// with `bitcoin_rpc_auth`. A success, or an unknown transaction, answered to a
/// lookup from the brc20 indexing thread is buffered under the next block's
/// height; every other answer is returned unbuffered. In light mode a miss is
/// an error, since a light client has no bitcoind. `network()` asks bitcoind's
/// `getblockchaininfo` past the cache in full mode and reports `NETWORK_TYPE`
/// in light mode.
///
/// Each lookup runs on a worker on the source's own runtime, with its own
/// Postgres pool and HTTP client, while the calling thread waits on a std
/// channel for at most 20 seconds. Lookups therefore complete from any thread
/// outside that runtime, including a current-thread runtime's, and never take
/// the brc20 database lock.
pub struct CachedBitcoinRpc {
    lookups: mpsc::UnboundedSender<Lookup>,
    slot: BitcoinRpcBufferSlot,
    /// The bound on one lookup, on both sides of the channel.
    timeout: Duration,
    light_client_mode: bool,
    network: Network,
    /// Drives the worker, its pool and its client; dropping it stops them.
    _runtime: ServiceRuntime,
}

impl CachedBitcoinRpc {
    /// Starts the source's runtime and worker for `config`.
    pub fn new(config: &Brc20IndexerConfig) -> anyhow::Result<Self> {
        Self::start(config, LOOKUP_TIMEOUT)
    }

    /// Starts the source with lookups bounded by `timeout`.
    fn start(config: &Brc20IndexerConfig, timeout: Duration) -> anyhow::Result<Self> {
        let runtime = ServiceRuntime::new("brc20-bitcoin-rpc")?;
        let handle = runtime.handle();
        let slot = BitcoinRpcBufferSlot::default();
        let worker = {
            // The pool and the client belong to the source's runtime.
            let _context = handle.enter();
            Arc::new(Worker {
                pool: connect_pool(config, 2, Some(POOL_ACQUIRE_TIMEOUT)),
                slot: slot.clone(),
                client: Client::builder().timeout(BITCOIND_TIMEOUT).build()?,
                bitcoin_rpc_url: config.bitcoin_rpc_url.clone(),
                auth: config.bitcoin_rpc_auth.clone(),
                light_client_mode: config.light_client_mode,
                cache_enabled: config.bitcoin_rpc_cache_enabled,
                first_inscription_height: config.first_inscription_height,
                next_id: AtomicU64::new(1),
            })
        };
        let (lookups, mut queue) = mpsc::unbounded_channel::<Lookup>();
        handle.spawn(async move {
            while let Some(lookup) = queue.recv().await {
                let worker = worker.clone();
                tokio::spawn(async move {
                    let answer = worker.answer(&lookup.request, lookup.cached, lookup.writer);
                    let answer = tokio::time::timeout(timeout, answer)
                        .await
                        .unwrap_or_else(|_| Err(timed_out(timeout)));
                    lookup.answer.send(answer).ok();
                });
            }
        });
        Ok(CachedBitcoinRpc {
            lookups,
            slot,
            timeout,
            light_client_mode: config.light_client_mode,
            network: config.network_type,
            _runtime: runtime,
        })
    }

    /// The slot holding the buffer lookups use.
    pub fn buffer_slot(&self) -> &BitcoinRpcBufferSlot {
        &self.slot
    }

    /// bitcoind's answer to `request` (a cache key), read through the cache
    /// when `cached`. Blocks the calling thread until the worker answers or
    /// the lookup times out.
    fn lookup(&self, request: Value, cached: bool) -> Result<Bytes, BitcoinRpcError> {
        let (answer, answered) = std::sync::mpsc::channel();
        self.lookups
            .send(Lookup {
                request,
                cached,
                writer: on_indexing_thread(),
                answer,
            })
            .map_err(|_| "the Bitcoin RPC source has stopped")?;
        match answered.recv_timeout(self.timeout) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(timed_out(self.timeout)),
            Err(RecvTimeoutError::Disconnected) => {
                Err("the Bitcoin RPC source dropped a lookup".into())
            }
        }
    }
}

/// One lookup for the worker, answered on `answer`. `writer` is whether it
/// comes from the indexing thread.
struct Lookup {
    request: Value,
    cached: bool,
    writer: bool,
    answer: std::sync::mpsc::Sender<Result<Bytes, BitcoinRpcError>>,
}

/// What the worker answers lookups with. The pool and the client belong to
/// the source's runtime.
struct Worker {
    pool: Pool<Postgres>,
    slot: BitcoinRpcBufferSlot,
    client: Client,
    bitcoin_rpc_url: String,
    /// Resolved on every request to bitcoind.
    auth: BitcoinRpcAuth,
    light_client_mode: bool,
    cache_enabled: bool,
    first_inscription_height: i32,
    next_id: AtomicU64,
}

impl Worker {
    async fn answer(
        &self,
        request: &Value,
        cached: bool,
        writer: bool,
    ) -> Result<Bytes, BitcoinRpcError> {
        let cache = cached.then(|| {
            BitcoinRpcCache::new(
                self.pool.clone(),
                self.slot.current(),
                self.cache_enabled,
                self.first_inscription_height,
            )
        });
        self.answer_with(cache.as_ref().map(|c| c as &dyn CacheRows), request, writer)
            .await
    }

    /// `answer` over `cache`, which is `None` for a lookup past the cache.
    async fn answer_with(
        &self,
        cache: Option<&dyn CacheRows>,
        request: &Value,
        writer: bool,
    ) -> Result<Bytes, BitcoinRpcError> {
        if let Some(cache) = cache {
            let key = serde_json::to_vec(request)?;
            match cache.buffered(&key).await {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(e) => return Err(cache_error("reading", e)),
            }
            match cache.stored(&key).await {
                Ok(Some(response)) if serves_stored(self.light_client_mode, request, &response) => {
                    return Ok(response);
                }
                Ok(_) => {}
                Err(e) => return Err(cache_error("reading", e)),
            }
        }
        if self.light_client_mode {
            return Err(format!("light mode has no cached answer to {request}").into());
        }

        let mut call = request.clone();
        call["id"] = self.next_id.fetch_add(1, Ordering::Relaxed).into();
        let body = serde_json::to_vec(&call)?;
        let post = self
            .client
            .post(&self.bitcoin_rpc_url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        let response = self.auth.authorize(post).await?.send().await?;
        let response = response.bytes().await?;
        if let Some(cache) = cache
            && buffers(writer, request, &response)
            && let Err(e) = cache.cache(&body, &response).await
        {
            return Err(cache_error("writing", e));
        }
        Ok(response)
    }
}

/// The cache a lookup reads and writes: the buffer of this block's lookups,
/// then the stored rows.
#[async_trait]
trait CacheRows: Send + Sync {
    /// The answer in the buffer.
    async fn buffered(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>>;
    /// The newest stored answer.
    async fn stored(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>>;
    /// Buffers bitcoind's `response` to `request`.
    async fn cache(&self, request: &[u8], response: &[u8]) -> Result<(), Box<dyn Error>>;
}

#[async_trait]
impl CacheRows for BitcoinRpcCache {
    async fn buffered(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
        self.get_buffered(request)
    }

    async fn stored(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
        self.get_stored(request).await
    }

    async fn cache(&self, request: &[u8], response: &[u8]) -> Result<(), Box<dyn Error>> {
        BitcoinRpcCache::cache(self, request, response).await
    }
}

/// Whether a stored row's `response` to `request` answers a lookup. Light mode
/// replays every row. Full mode skips a "not yet" row, which was true only at
/// the block that asked: a later block asks bitcoind again and buffers that
/// answer under its own height.
fn serves_stored(light_client_mode: bool, request: &Value, response: &[u8]) -> bool {
    if light_client_mode {
        return true;
    }
    match serde_json::from_slice::<Value>(response) {
        Ok(response) => !is_not_yet(request, &response),
        // An unreadable row is not served; bitcoind answers instead.
        Err(_) => false,
    }
}

/// Whether `response` to `request` is a "not yet" answer: `getrawtransaction`
/// with error -5 at either verbosity, or with verbose=true and a result that
/// has no `blockhash` (known to the node but unconfirmed). A found
/// verbose=false answer has no block hash and its body cannot change.
fn is_not_yet(request: &Value, response: &Value) -> bool {
    if request["method"] != json!("getrawtransaction") {
        return false;
    }
    match response.get("error") {
        None | Some(Value::Null) => {
            request["params"][1] == json!(true) && response["result"]["blockhash"].is_null()
        }
        Some(error) => error["code"] == json!(NOT_FOUND),
    }
}

/// Whether bitcoind's answer `body` to `request` goes into the buffer: only
/// for the indexing thread (`writer`), and only a success or an unknown
/// transaction, since both stay true for the chain the cache is published
/// for. Any other error, transient ones included, is returned unbuffered.
fn buffers(writer: bool, request: &Value, body: &[u8]) -> bool {
    if !writer {
        return false;
    }
    let Ok(response) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    match response.get("error") {
        None | Some(Value::Null) => true,
        Some(error) => {
            error["code"] == json!(NOT_FOUND) && request["method"] == json!("getrawtransaction")
        }
    }
}

/// Whether the calling thread is brc20's indexing thread. Lookups from other
/// threads (the public JSON-RPC's workers) read the cache but never write it.
fn on_indexing_thread() -> bool {
    std::thread::current().name() == Some(BRC20_THREAD_NAME)
}

fn timed_out(timeout: Duration) -> BitcoinRpcError {
    format!("the Bitcoin RPC lookup took longer than {timeout:?}").into()
}

fn cache_error(action: &str, error: Box<dyn Error>) -> BitcoinRpcError {
    format!("{action} the Bitcoin RPC cache failed: {error}").into()
}

/// A cache key: the request bitcoincore_rpc sends, without its `id`.
fn request(method: &str, params: Vec<Value>) -> Value {
    json!({ "method": method, "params": params, "jsonrpc": "2.0" })
}

/// `get_raw_transaction(txid, None)` with `verbose` false, or
/// `get_raw_transaction_info(txid, None)` with `verbose` true.
fn raw_transaction_request(txid: &Txid, verbose: bool) -> Value {
    request("getrawtransaction", vec![json!(txid), json!(verbose)])
}

/// `get_block_header_info(hash)`.
fn block_header_request(hash: &BlockHash) -> Value {
    request("getblockheader", vec![json!(hash), json!(true)])
}

/// `get_blockchain_info()`.
fn blockchain_info_request() -> Value {
    request("getblockchaininfo", vec![])
}

/// The result in `body`, read as bitcoincore_rpc reads a response. A cached
/// response is stored without the `id` that jsonrpc's response requires.
fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, jsonrpc::Error> {
    let mut response: Value = serde_json::from_slice(body).map_err(jsonrpc::Error::Json)?;
    if let Some(object) = response.as_object_mut() {
        object.entry("id").or_insert(Value::Null);
    }
    let response = serde_json::to_vec(&response).map_err(jsonrpc::Error::Json)?;
    serde_json::from_slice::<jsonrpc::Response>(&response)
        .map_err(jsonrpc::Error::Json)?
        .result()
}

/// Maps an unknown transaction to `None`, as `BitcoinCoreRpc` does.
fn not_found_as_none<T>(result: Result<T, jsonrpc::Error>) -> Result<Option<T>, BitcoinRpcError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(jsonrpc::Error::Rpc(ref error)) if error.code == NOT_FOUND => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl BitcoinRpc for CachedBitcoinRpc {
    fn get_transaction(&self, txid: &Txid) -> Result<Option<Transaction>, BitcoinRpcError> {
        let body = self.lookup(raw_transaction_request(txid, false), true)?;
        match not_found_as_none(parse::<String>(&body))? {
            Some(hex) => Ok(Some(encode::deserialize_hex(&hex)?)),
            None => Ok(None),
        }
    }

    fn get_transaction_with_block_hash(
        &self,
        txid: &Txid,
    ) -> Result<Option<(Transaction, Option<BlockHash>)>, BitcoinRpcError> {
        let body = self.lookup(raw_transaction_request(txid, true), true)?;
        let Some(info) = not_found_as_none(parse::<GetRawTransactionResult>(&body))? else {
            return Ok(None);
        };
        // An undecodable transaction fails the precompile call like a missing one.
        Ok(info.transaction().ok().map(|tx| (tx, info.blockhash)))
    }

    fn get_block_height(&self, block_hash: &BlockHash) -> Result<usize, BitcoinRpcError> {
        let body = self.lookup(block_header_request(block_hash), true)?;
        Ok(parse::<GetBlockHeaderResult>(&body)?.height)
    }

    fn network(&self) -> Result<Network, BitcoinRpcError> {
        if self.light_client_mode {
            return Ok(self.network);
        }
        let body = self.lookup(blockchain_info_request(), false)?;
        Ok(parse::<GetBlockchainInfoResult>(&body)?.chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::BitcoinRpcAuth,
        database::{
            BitcoinRpcResultRecord, Brc20Database, get_brc20_database, replace_brc20_database,
        },
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        str::FromStr,
    };

    /// Rows of a signet node's `brc20_bitcoin_rpc_result_cache`, keyed by
    /// bitcoincore_rpc 0.19's requests.
    const SIGNET_ROWS: &str = include_str!("testdata/bitcoin_rpc_cache_signet.json");

    const TX: &str = "7499c1a1627d28bd918f751a2a4a237d6de016b6b4deafef646c60b0d257ee14";
    const TX_INFO: &str = "2caca782ace3e3267bf854afa29609a6ea4c7d072fbe9ee466e0a9358b1dccaf";
    const TX_INFO_BLOCK: &str = "0000000d645de35555b305bf9d264b2b31b9ebc5c5f0972f492d8307437ad164";
    const HEADER: &str = "000000006b91b301ef3f223e72ccf1baeebe62418c2dfcee7664773c57267bcf";

    fn signet_rows() -> Vec<Value> {
        serde_json::from_str(SIGNET_ROWS).unwrap()
    }

    /// The stored response of the signet row whose request is `request`.
    fn signet_response(request: &Value) -> Value {
        signet_rows()
            .into_iter()
            .find(|row| &row["request"] == request)
            .map(|row| row["response"].clone())
            .unwrap_or_else(|| panic!("no signet row for {request}"))
    }

    fn txid(hex: &str) -> Txid {
        Txid::from_str(hex).unwrap()
    }

    fn block_hash(hex: &str) -> BlockHash {
        BlockHash::from_str(hex).unwrap()
    }

    /// What a bitcoind stand-in saw: each request body and its Authorization.
    type Seen = Arc<Mutex<Vec<(Value, Option<String>)>>>;

    /// A bitcoind stand-in on its own thread that answers each request with
    /// `answer(request)`. Returns its URL and what it saw.
    fn stub_bitcoind(answer: impl Fn(&Value) -> String + Send + 'static) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Seen::default();
        let recorded = seen.clone();
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
                let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let response = answer(&request);
                recorded.lock().unwrap().push((request, authorization));
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .ok();
            }
        });
        (url, seen)
    }

    /// `response` answering `request`, carrying the request's id.
    fn reply(request: &Value, mut response: Value) -> String {
        response["id"] = request["id"].clone();
        response.to_string()
    }

    fn rpc_error(request: &Value, code: i32) -> String {
        reply(
            request,
            json!({ "jsonrpc": "2.0", "error": { "code": code, "message": "stub" } }),
        )
    }

    /// The request without its id, as the cache keys it.
    fn without_id(request: &Value) -> Value {
        let mut request = request.clone();
        request.as_object_mut().unwrap().remove("id");
        request
    }

    fn config(url: &str, light_client_mode: bool, cache: bool) -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            bitcoin_rpc_url: url.to_string(),
            bitcoin_rpc_auth: BitcoinRpcAuth::None,
            light_client_mode,
            bitcoin_rpc_cache_enabled: cache,
            network_type: Network::Signet,
            network_type_string: "signet".to_string(),
            ..Default::default()
        }
    }

    /// A buffer holding `response` for each request.
    fn buffer_of(entries: impl IntoIterator<Item = (Value, Value)>) -> BitcoinRpcBuffer {
        let buffer = BitcoinRpcBuffer::default();
        for (request, response) in entries {
            let method = request["method"].as_str().unwrap().to_string();
            buffer.lock().unwrap().insert(
                request,
                BitcoinRpcResultRecord {
                    method,
                    response,
                    block_height: 1,
                },
            );
        }
        buffer
    }

    /// A full-mode source with the cache off, in front of a bitcoind that
    /// answers `getrawtransaction` for `TX` from the signet row.
    fn source_for_tx() -> (CachedBitcoinRpc, Seen) {
        let (url, seen) = stub_bitcoind(|request| {
            reply(
                request,
                signet_response(&raw_transaction_request(&txid(TX), false)),
            )
        });
        (
            CachedBitcoinRpc::new(&config(&url, false, false)).unwrap(),
            seen,
        )
    }

    fn assert_is_tx(tx: Option<Transaction>) {
        assert_eq!(tx.expect("the transaction exists").compute_txid(), txid(TX));
    }

    #[test]
    fn keys_match_the_signet_cache_rows() {
        let rows = signet_rows();
        assert_eq!(rows.len(), 4);
        for row in rows {
            let stored = &row["request"];
            let params = &stored["params"];
            let key = match row["method"].as_str().unwrap() {
                "getrawtransaction" => raw_transaction_request(
                    &txid(params[0].as_str().unwrap()),
                    params[1].as_bool().unwrap(),
                ),
                "getblockheader" => block_header_request(&block_hash(params[0].as_str().unwrap())),
                "getblockchaininfo" => blockchain_info_request(),
                method => panic!("unexpected method {method}"),
            };
            // The buffer compares keys as values (Postgres as jsonb, which
            // ignores key order), so equal values find the same entry.
            assert_eq!(&key, stored);
            let buffered = HashMap::from([(stored.clone(), ())]);
            assert!(buffered.contains_key(&key), "{key}");
        }
    }

    #[test]
    fn buffered_signet_rows_are_served_without_bitcoind() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -1));
        let source = CachedBitcoinRpc::new(&config(&url, false, true)).unwrap();
        source.buffer_slot().install(buffer_of(
            signet_rows()
                .into_iter()
                .map(|row| (row["request"].clone(), row["response"].clone())),
        ));

        assert_is_tx(source.get_transaction(&txid(TX)).unwrap());
        let (tx, block) = source
            .get_transaction_with_block_hash(&txid(TX_INFO))
            .unwrap()
            .expect("the transaction exists");
        assert_eq!(tx.compute_txid(), txid(TX_INFO));
        assert_eq!(block, Some(block_hash(TX_INFO_BLOCK)));
        assert_eq!(
            source.get_block_height(&block_hash(HEADER)).unwrap(),
            250_266
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_request_is_the_cache_key_with_an_id() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -5));
        let source = CachedBitcoinRpc::new(&config(&url, false, false)).unwrap();
        source.get_transaction(&txid(TX)).unwrap();
        source.get_transaction_with_block_hash(&txid(TX)).unwrap();
        source.get_block_height(&block_hash(HEADER)).unwrap_err();

        let seen = seen.lock().unwrap();
        let requests: Vec<_> = seen.iter().map(|(request, _)| request).collect();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request["id"].is_number()));
        assert_eq!(
            without_id(requests[0]),
            raw_transaction_request(&txid(TX), false)
        );
        assert_eq!(
            without_id(requests[1]),
            raw_transaction_request(&txid(TX), true)
        );
        assert_eq!(
            without_id(requests[2]),
            block_header_request(&block_hash(HEADER))
        );
        assert!(
            seen.iter()
                .all(|(_, authorization)| authorization.is_none())
        );
    }

    #[test]
    fn not_found_is_none_for_transactions_and_an_error_for_headers() {
        let (url, _) = stub_bitcoind(|request| rpc_error(request, -5));
        let source = CachedBitcoinRpc::new(&config(&url, false, false)).unwrap();
        assert!(source.get_transaction(&txid(TX)).unwrap().is_none());
        assert!(
            source
                .get_transaction_with_block_hash(&txid(TX))
                .unwrap()
                .is_none()
        );
        assert!(source.get_block_height(&block_hash(HEADER)).is_err());
    }

    #[test]
    fn other_rpc_errors_and_unreadable_answers_are_errors() {
        let answers: [fn(&Value) -> String; 2] = [
            |request| rpc_error(request, -8),
            |_| "<html>work queue depth exceeded</html>".to_string(),
        ];
        for answer in answers {
            let (url, seen) = stub_bitcoind(answer);
            let source = CachedBitcoinRpc::new(&config(&url, false, false)).unwrap();
            assert!(source.get_transaction(&txid(TX)).is_err());
            assert!(source.get_transaction_with_block_hash(&txid(TX)).is_err());
            assert!(source.get_block_height(&block_hash(HEADER)).is_err());
            assert!(source.network().is_err());
            assert_eq!(seen.lock().unwrap().len(), 4);
        }
    }

    #[test]
    fn an_undecodable_transaction_is_an_error_raw_and_none_verbose() {
        let (url, _) = stub_bitcoind(|request| {
            if request["params"][1] == json!(true) {
                let mut response = signet_response(&raw_transaction_request(&txid(TX_INFO), true));
                response["result"]["hex"] = json!("00");
                reply(request, response)
            } else {
                reply(request, json!({ "jsonrpc": "2.0", "result": "00" }))
            }
        });
        let source = CachedBitcoinRpc::new(&config(&url, false, false)).unwrap();
        assert!(source.get_transaction(&txid(TX_INFO)).is_err());
        assert!(
            source
                .get_transaction_with_block_hash(&txid(TX_INFO))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_unreachable_bitcoind_is_an_error() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let source = CachedBitcoinRpc::new(&config(&url, false, false)).unwrap();
        assert!(source.get_transaction(&txid(TX)).is_err());
        assert!(source.network().is_err());
    }

    #[test]
    fn a_cached_error_is_served_like_a_live_one() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -1));
        let source = CachedBitcoinRpc::new(&config(&url, false, true)).unwrap();
        let error =
            |code: i32| json!({ "jsonrpc": "2.0", "error": { "code": code, "message": "x" } });
        source.buffer_slot().install(buffer_of([
            (raw_transaction_request(&txid(TX), false), error(-5)),
            (raw_transaction_request(&txid(TX), true), error(-8)),
        ]));
        assert!(source.get_transaction(&txid(TX)).unwrap().is_none());
        assert!(source.get_transaction_with_block_hash(&txid(TX)).is_err());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_installed_buffer_is_read_per_lookup() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -5));
        let source = CachedBitcoinRpc::new(&config(&url, false, true)).unwrap();
        let request = raw_transaction_request(&txid(TX), false);
        source
            .buffer_slot()
            .install(buffer_of([(request.clone(), signet_response(&request))]));
        assert_is_tx(source.get_transaction(&txid(TX)).unwrap());
        source.buffer_slot().install(buffer_of([(
            request,
            json!({ "jsonrpc": "2.0", "error": { "code": -5, "message": "x" } }),
        )]));
        assert!(source.get_transaction(&txid(TX)).unwrap().is_none());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn light_mode_answers_from_the_cache_and_never_calls_bitcoind() {
        let (url, seen) = stub_bitcoind(|request| {
            reply(
                request,
                signet_response(&raw_transaction_request(&txid(TX), false)),
            )
        });
        // A disabled cache misses without reaching Postgres.
        let source = CachedBitcoinRpc::new(&config(&url, true, false)).unwrap();
        assert!(source.get_transaction(&txid(TX)).is_err());
        assert!(source.get_transaction_with_block_hash(&txid(TX)).is_err());
        assert!(source.get_block_height(&block_hash(HEADER)).is_err());
        assert_eq!(source.network().unwrap(), Network::Signet);

        let source = CachedBitcoinRpc::new(&config(&url, true, true)).unwrap();
        let request = raw_transaction_request(&txid(TX), false);
        source
            .buffer_slot()
            .install(buffer_of([(request.clone(), signet_response(&request))]));
        assert_is_tx(source.get_transaction(&txid(TX)).unwrap());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn light_mode_reports_the_configured_network() {
        let source = CachedBitcoinRpc::new(&Brc20IndexerConfig {
            network_type: Network::Bitcoin,
            network_type_string: "mainnet".to_string(),
            ..config("http://127.0.0.1:1", true, false)
        })
        .unwrap();
        assert_eq!(source.network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn full_mode_network_asks_bitcoind_past_the_cache() {
        let (url, seen) =
            stub_bitcoind(|request| reply(request, signet_response(&blockchain_info_request())));
        let source = CachedBitcoinRpc::new(&Brc20IndexerConfig {
            network_type: Network::Bitcoin,
            network_type_string: "mainnet".to_string(),
            ..config(&url, false, true)
        })
        .unwrap();
        let mut main = signet_response(&blockchain_info_request());
        main["result"]["chain"] = json!("main");
        source
            .buffer_slot()
            .install(buffer_of([(blockchain_info_request(), main)]));

        assert_eq!(source.network().unwrap(), Network::Signet);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(without_id(&seen[0].0), blockchain_info_request());
    }

    #[test]
    fn bitcoind_sees_the_configured_credentials() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -5));
        let source = CachedBitcoinRpc::new(&Brc20IndexerConfig {
            bitcoin_rpc_auth: BitcoinRpcAuth::UserPass("rpcuser".into(), "rpcpass".into()),
            ..config(&url, false, false)
        })
        .unwrap();
        source.get_transaction(&txid(TX)).unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].1.as_deref(),
            Some("Basic cnBjdXNlcjpycGNwYXNz")
        );
    }

    #[tokio::test]
    async fn a_lookup_from_a_current_thread_runtime_completes() {
        let (source, seen) = source_for_tx();
        assert_is_tx(source.get_transaction(&txid(TX)).unwrap());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lookup_from_a_multi_thread_runtime_worker_completes() {
        let (source, _) = source_for_tx();
        let tx = tokio::spawn(async move { source.get_transaction(&txid(TX)) })
            .await
            .unwrap();
        assert_is_tx(tx.unwrap());
    }

    #[tokio::test]
    async fn a_lookup_while_the_brc20_database_is_locked_completes() {
        let (source, _) = source_for_tx();
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        replace_brc20_database(Brc20Database::new(&config(
            "http://127.0.0.1:1",
            false,
            false,
        )))
        .await;
        let database = get_brc20_database();
        let _locked = database.lock().await;

        let (done, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            done.send(source.get_transaction(&txid(TX)).map_err(|e| e.to_string()))
                .ok();
        });
        let tx = answer
            .recv_timeout(Duration::from_secs(20))
            .expect("the lookup completes while the lock is held");
        assert_is_tx(tx.unwrap());
    }

    #[test]
    fn the_indexer_buffers_successes_and_unknown_transactions_only() {
        let tx = raw_transaction_request(&txid(TX), false);
        let header = block_header_request(&block_hash(HEADER));
        let error = |code: i32| {
            json!({ "jsonrpc": "2.0", "error": { "code": code, "message": "x" } }).to_string()
        };
        let success = signet_response(&tx).to_string();

        assert!(buffers(true, &tx, success.as_bytes()));
        assert!(buffers(
            true,
            &tx,
            br#"{"result":"00","error":null,"id":1}"#
        ));
        assert!(buffers(true, &tx, error(-5).as_bytes()));
        assert!(buffers(
            true,
            &raw_transaction_request(&txid(TX), true),
            error(-5).as_bytes()
        ));
        assert!(!buffers(true, &header, error(-5).as_bytes()));
        assert!(!buffers(true, &tx, error(-8).as_bytes()));
        assert!(!buffers(true, &tx, error(-28).as_bytes()));
        assert!(!buffers(
            true,
            &tx,
            b"<html>work queue depth exceeded</html>"
        ));
    }

    fn rpc_error_value(code: i32) -> Value {
        json!({ "jsonrpc": "2.0", "error": { "code": code, "message": "x" } })
    }

    /// The verbose `getrawtransaction` answer for `TX_INFO` without its block.
    fn unconfirmed_info() -> Value {
        let mut response = signet_response(&raw_transaction_request(&txid(TX_INFO), true));
        response["result"]
            .as_object_mut()
            .unwrap()
            .remove("blockhash");
        response
    }

    #[test]
    fn a_not_yet_answer_is_a_getrawtransaction_without_a_block() {
        let raw = raw_transaction_request(&txid(TX), false);
        let verbose = raw_transaction_request(&txid(TX_INFO), true);
        let header = block_header_request(&block_hash(HEADER));
        let confirmed = signet_response(&verbose);
        let found_raw = signet_response(&raw);

        // Unknown transaction, either verbosity.
        assert!(is_not_yet(&raw, &rpc_error_value(-5)));
        assert!(is_not_yet(&verbose, &rpc_error_value(-5)));
        // Known but unconfirmed.
        assert!(is_not_yet(&verbose, &unconfirmed_info()));
        let mut null_hash = confirmed.clone();
        null_hash["result"]["blockhash"] = Value::Null;
        assert!(is_not_yet(&verbose, &null_hash));
        // Confirmed.
        assert!(!is_not_yet(&verbose, &confirmed));
        // A found raw transaction carries no block hash and cannot change.
        assert!(!is_not_yet(&raw, &found_raw));
        // Other errors and other methods.
        assert!(!is_not_yet(&raw, &rpc_error_value(-8)));
        assert!(!is_not_yet(&verbose, &rpc_error_value(-28)));
        assert!(!is_not_yet(&header, &rpc_error_value(-5)));
        assert!(!is_not_yet(
            &header,
            &signet_response(&block_header_request(&block_hash(HEADER)))
        ));
    }

    #[test]
    fn light_mode_replays_not_yet_rows_and_full_mode_skips_them() {
        let raw = raw_transaction_request(&txid(TX), false);
        let verbose = raw_transaction_request(&txid(TX_INFO), true);
        let not_yet = rpc_error_value(-5).to_string();
        let unconfirmed = unconfirmed_info().to_string();
        let confirmed = signet_response(&verbose).to_string();
        let found_raw = signet_response(&raw).to_string();

        for (request, response) in [
            (&raw, &not_yet),
            (&verbose, &not_yet),
            (&verbose, &unconfirmed),
        ] {
            assert!(serves_stored(true, request, response.as_bytes()));
            assert!(!serves_stored(false, request, response.as_bytes()));
        }
        for (request, response) in [(&verbose, &confirmed), (&raw, &found_raw)] {
            assert!(serves_stored(true, request, response.as_bytes()));
            assert!(serves_stored(false, request, response.as_bytes()));
        }
        // An unreadable row in full mode goes to bitcoind.
        assert!(!serves_stored(false, &raw, b"<html>"));
    }

    /// A cache over a map, with a fixed stored row.
    #[derive(Default)]
    struct FakeRows {
        buffer: Mutex<HashMap<Vec<u8>, Bytes>>,
        stored: Mutex<Option<Bytes>>,
    }

    impl FakeRows {
        fn with_stored(response: Value) -> Self {
            let rows = FakeRows::default();
            *rows.stored.lock().unwrap() = Some(Bytes::from(response.to_string()));
            rows
        }

        fn buffered_count(&self) -> usize {
            self.buffer.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl CacheRows for FakeRows {
        async fn buffered(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
            Ok(self.buffer.lock().unwrap().get(request).cloned())
        }

        async fn stored(&self, _: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
            Ok(self.stored.lock().unwrap().clone())
        }

        async fn cache(&self, request: &[u8], response: &[u8]) -> Result<(), Box<dyn Error>> {
            let key = serde_json::to_vec(&without_id(&serde_json::from_slice(request)?))?;
            self.buffer
                .lock()
                .unwrap()
                .insert(key, Bytes::copy_from_slice(response));
            Ok(())
        }
    }

    /// A worker for `light_client_mode` in front of the bitcoind at `url`.
    fn worker(url: &str, light_client_mode: bool) -> Worker {
        let config = config(url, light_client_mode, true);
        Worker {
            pool: connect_pool(&config, 1, Some(Duration::from_millis(100))),
            slot: BitcoinRpcBufferSlot::default(),
            client: Client::new(),
            bitcoin_rpc_url: url.to_string(),
            auth: BitcoinRpcAuth::None,
            light_client_mode,
            cache_enabled: true,
            first_inscription_height: 0,
            next_id: AtomicU64::new(1),
        }
    }

    /// A bitcoind that has confirmed `TX_INFO`.
    fn confirmed_bitcoind() -> (String, Seen) {
        stub_bitcoind(|request| {
            reply(
                request,
                signet_response(&raw_transaction_request(&txid(TX_INFO), true)),
            )
        })
    }

    #[tokio::test]
    async fn full_mode_asks_bitcoind_past_a_not_yet_row_and_buffers_the_fresh_answer() {
        let (url, seen) = confirmed_bitcoind();
        let worker = worker(&url, false);
        let request = raw_transaction_request(&txid(TX_INFO), true);
        for not_yet in [rpc_error_value(-5), unconfirmed_info()] {
            seen.lock().unwrap().clear();
            let rows = FakeRows::with_stored(not_yet);

            let answer = worker
                .answer_with(Some(&rows), &request, true)
                .await
                .unwrap();
            let answer: Value = serde_json::from_slice(&answer).unwrap();
            assert_eq!(
                answer["result"]["blockhash"],
                json!(TX_INFO_BLOCK),
                "the fresh answer"
            );
            assert_eq!(seen.lock().unwrap().len(), 1);
            assert_eq!(rows.buffered_count(), 1);

            // The block's next lookup is served from the buffer.
            worker
                .answer_with(Some(&rows), &request, true)
                .await
                .unwrap();
            assert_eq!(seen.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn full_mode_serves_a_confirmed_row_without_bitcoind() {
        let (url, seen) = stub_bitcoind(|request| rpc_error(request, -1));
        let worker = worker(&url, false);
        let request = raw_transaction_request(&txid(TX_INFO), true);
        let rows = FakeRows::with_stored(signet_response(&request));
        let answer = worker
            .answer_with(Some(&rows), &request, true)
            .await
            .unwrap();
        let answer: Value = serde_json::from_slice(&answer).unwrap();
        assert_eq!(answer["result"]["blockhash"], json!(TX_INFO_BLOCK));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(rows.buffered_count(), 0);
    }

    #[tokio::test]
    async fn full_mode_does_not_buffer_a_fresh_answer_off_the_indexing_thread() {
        let (url, seen) = confirmed_bitcoind();
        let worker = worker(&url, false);
        let request = raw_transaction_request(&txid(TX_INFO), true);
        let rows = FakeRows::with_stored(rpc_error_value(-5));
        worker
            .answer_with(Some(&rows), &request, false)
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(rows.buffered_count(), 0);
    }

    #[tokio::test]
    async fn light_mode_serves_a_not_yet_row_without_bitcoind() {
        let (url, seen) = confirmed_bitcoind();
        let worker = worker(&url, true);
        let request = raw_transaction_request(&txid(TX_INFO), true);
        let rows = FakeRows::with_stored(rpc_error_value(-5));
        let answer = worker
            .answer_with(Some(&rows), &request, true)
            .await
            .unwrap();
        let answer: Value = serde_json::from_slice(&answer).unwrap();
        assert_eq!(answer["error"]["code"], json!(NOT_FOUND));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn other_threads_never_buffer() {
        let tx = raw_transaction_request(&txid(TX), false);
        let success = signet_response(&tx).to_string();
        assert!(!buffers(false, &tx, success.as_bytes()));
    }

    #[test]
    fn only_the_brc20_thread_is_the_indexing_thread() {
        use crate::fail_stop::{BRC20_THREAD_NAME, RPC_THREAD_NAME};
        let on = |name: Option<&str>| {
            let builder = std::thread::Builder::new();
            let builder = match name {
                Some(name) => builder.name(name.to_string()),
                None => builder,
            };
            builder.spawn(on_indexing_thread).unwrap().join().unwrap()
        };
        assert!(on(Some(BRC20_THREAD_NAME)));
        assert!(!on(Some(RPC_THREAD_NAME)));
        assert!(!on(Some("brc20-bitcoin-rpc")));
        assert!(!on(None));
    }

    #[test]
    fn a_lookup_bitcoind_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            // Holds every connection open without answering.
            let held: Vec<_> = listener.incoming().flatten().collect();
            drop(held);
        });
        let source =
            CachedBitcoinRpc::start(&config(&url, false, false), Duration::from_millis(300))
                .unwrap();
        let started = std::time::Instant::now();
        assert!(source.get_transaction(&txid(TX)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
