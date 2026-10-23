#![allow(dead_code)]

use {
    axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    },
    bitcoin::{
        Address, Block, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid, Witness,
        consensus::encode::deserialize_hex,
        hashes::Hash as _,
        opcodes,
        script::{self, PushBytesBuf},
    },
    mockcore::TransactionTemplate,
    opi_core::BlockSource,
    opi_source::{
        driver::{SourceConfig, SourceHandle, spawn_source},
        journal::{Journal, JournalReader, cf, table},
        proxy::UpstreamAuth,
    },
    ord::{Chain, InscriptionId},
    redb::{ReadableDatabase, ReadableTable},
    serde_json::{Value, json},
    std::{
        path::Path,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    },
};

/// A mock bitcoind plus helpers that build ord inscription transactions on it.
pub struct Harness {
    pub core: mockcore::Handle,
}

impl Harness {
    /// Starts a regtest mock bitcoind.
    pub fn new() -> Harness {
        Harness {
            core: mockcore::builder().network(Network::Regtest).build(),
        }
    }

    /// Mines a funding block, then broadcasts (without mining) a reveal
    /// transaction inscribing `body`. Returns the id of the new inscription
    /// (`<reveal txid>i0`). Call `mine` to confirm it.
    pub fn inscribe(&self, body: &[u8], content_type: &str) -> InscriptionId {
        self.inscribe_witness(envelope(content_type.as_bytes(), body))
    }

    /// Like [`Harness::inscribe`], with the reveal input carrying `witness`.
    pub fn inscribe_witness(&self, witness: Witness) -> InscriptionId {
        self.core.mine_blocks(1);
        self.reveal(usize::try_from(self.core.height()).unwrap(), witness)
    }

    /// Broadcasts (without mining) a reveal transaction spending the coinbase
    /// of block `funding_height` with `witness`, to one output.
    pub fn reveal(&self, funding_height: usize, witness: Witness) -> InscriptionId {
        let txid = self.core.broadcast_tx(TransactionTemplate {
            inputs: &[(funding_height, 0, 0, witness)],
            ..Default::default()
        });
        InscriptionId { txid, index: 0 }
    }

    /// Like [`Harness::reveal`], with no outputs, so the whole value and the
    /// inscription go to fees.
    pub fn reveal_to_fee(&self, funding_height: usize, witness: Witness) -> InscriptionId {
        let txid = self.core.broadcast_tx(TransactionTemplate {
            inputs: &[(funding_height, 0, 0, witness)],
            outputs: 0,
            ..Default::default()
        });
        InscriptionId { txid, index: 0 }
    }

    /// Broadcasts (without mining) a transaction spending `outpoint` to a single
    /// new output, paying `fee`. `outpoint` must be confirmed.
    pub fn transfer(&self, outpoint: OutPoint, fee: u64) -> Txid {
        let (block, tx) = self.core.tx_index(outpoint.txid);
        self.core.broadcast_tx(TransactionTemplate {
            fee,
            inputs: &[(block, tx, outpoint.vout as usize, Witness::new())],
            ..Default::default()
        })
    }

    /// Broadcasts (without mining) a transaction spending `outpoint` to a single
    /// output paying `script`, with no fee.
    pub fn transfer_to(&self, outpoint: OutPoint, script: &ScriptBuf) -> Txid {
        let (block, tx) = self.core.tx_index(outpoint.txid);
        self.core.broadcast_tx(TransactionTemplate {
            inputs: &[(block, tx, outpoint.vout as usize, Witness::new())],
            recipient: Some(Address::from_script(script, Network::Regtest).unwrap()),
            ..Default::default()
        })
    }

    /// Broadcasts (without mining) a transaction spending `outpoint` with no
    /// outputs, so its whole value goes to fees.
    pub fn spend_to_fee(&self, outpoint: OutPoint) -> Txid {
        let (block, tx) = self.core.tx_index(outpoint.txid);
        self.core.broadcast_tx(TransactionTemplate {
            inputs: &[(block, tx, outpoint.vout as usize, Witness::new())],
            outputs: 0,
            ..Default::default()
        })
    }

    /// Adds to the mempool a transaction spending `outpoint`, confirmed or in
    /// the mempool, to one output of its whole value. mockcore's templates
    /// only spend confirmed outputs.
    pub fn spend_in_mempool(&self, outpoint: OutPoint) -> Txid {
        let mut state = self.core.state();
        let value = state
            .mempool
            .iter()
            .chain(state.transactions.values())
            .find(|tx| tx.compute_txid() == outpoint.txid)
            .map(|tx| tx.output[outpoint.vout as usize].value)
            .unwrap();
        let tx = Transaction {
            version: bitcoin::transaction::Version(2),
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: outpoint,
                script_sig: ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![bitcoin::TxOut {
                value,
                script_pubkey: ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array(
                    [9; 20],
                )),
            }],
        };
        let txid = tx.compute_txid();
        state.mempool.push(tx);
        txid
    }

    /// Marks `outpoint` unspent again. mockcore's `invalidate_tip` leaves the
    /// outputs the dropped block spent marked spent.
    pub fn restore_utxo(&self, outpoint: OutPoint) {
        let mut state = self.core.state();
        let value = state.transactions[&outpoint.txid].output[outpoint.vout as usize].value;
        state.utxos.insert(outpoint, value);
    }

    /// Mines `n` blocks, confirming everything in the mempool in the first.
    pub fn mine(&self, n: usize) -> Vec<Block> {
        self.core.mine_blocks(n as u64)
    }

    /// Mines one block whose coinbase pays `subsidy` plus fees. A subsidy
    /// below the chain's leaves the difference, and any inscription on it,
    /// lost.
    pub fn mine_with_subsidy(&self, subsidy: u64) -> Block {
        self.core.mine_blocks_with_subsidy(1, subsidy).remove(0)
    }

    /// Removes the tip block from the chain.
    pub fn invalidate_tip(&self) -> BlockHash {
        self.core.invalidate_tip()
    }

    pub fn height(&self) -> u32 {
        u32::try_from(self.core.height()).unwrap()
    }
}

/// Tapscript witness `OP_FALSE OP_IF "ord" 1 <content_type> 0 <body> OP_ENDIF`.
pub fn envelope(content_type: &[u8], body: &[u8]) -> Witness {
    envelope_with(&[(&[1], content_type)], body)
}

/// Tapscript witness `OP_FALSE OP_IF "ord" (<tag> <value>)* 0 <body> OP_ENDIF`.
pub fn envelope_with(fields: &[(&[u8], &[u8])], body: &[u8]) -> Witness {
    let push = |data: &[u8]| {
        let mut buf = PushBytesBuf::new();
        buf.extend_from_slice(data).unwrap();
        buf
    };

    let mut builder = script::Builder::new()
        .push_opcode(opcodes::OP_FALSE)
        .push_opcode(opcodes::all::OP_IF)
        .push_slice(push(b"ord"));

    for (tag, value) in fields {
        builder = builder.push_slice(push(tag)).push_slice(push(value));
    }

    builder = builder.push_slice(push(&[]));

    for chunk in body.chunks(520) {
        builder = builder.push_slice(push(chunk));
    }

    let script: ScriptBuf = builder.push_opcode(opcodes::all::OP_ENDIF).into_script();

    Witness::from_slice(&[script.into_bytes(), Vec::new()])
}

/// Source config for a regtest run under `dir`, reading bitcoind at `url`.
pub fn source_config(dir: &Path, url: String, chunk: u32) -> SourceConfig {
    SourceConfig {
        chain: Chain::Regtest,
        ord_data_dir: dir.join("ord"),
        journal_dir: dir.join("journal"),
        bitcoin_rpc_url: url,
        bitcoin_auth: UpstreamAuth::UserPass("user".into(), "pass".into()),
        chunk,
    }
}

/// Starts a source with `chunk = 2` under `dir` against the harness.
pub fn start(harness: &Harness, dir: &Path) -> SourceHandle {
    spawn_source(source_config(dir, harness.core.url(), 2)).unwrap()
}

/// Waits until the reader's tip reaches `height`.
pub fn wait_for_tip(reader: &JournalReader, height: u32) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tip = reader.tip().unwrap();
        if tip.is_some_and(|tip| tip >= height) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "journal tip {tip:?} never reached {height}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every `TRANSFERS`, `INFO` and `HEADERS` row of the journal at `path`.
pub fn dump(path: &Path) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let journal = Journal::open(path, Chain::Regtest).unwrap();
    let tx = journal.db().begin_read().unwrap();
    let mut rows = Vec::new();
    for name in [cf::TRANSFERS, cf::INFO, cf::HEADERS] {
        for row in tx.open_table(table(name)).unwrap().iter().unwrap() {
            let (key, value) = row.unwrap();
            rows.push((name, key.value().to_vec(), value.value().to_vec()));
        }
    }
    rows
}

/// Faults the [`Shim`] injects.
#[derive(Default)]
pub struct Faults {
    /// Answers HTTP 500 to this many `getblock` calls for the hash.
    pub fail_getblock: Option<(BlockHash, usize)>,
    /// Passes the first `.0` `getblockchaininfo` calls, then answers HTTP 500
    /// to the next `.1`.
    pub fail_blockchaininfo: Option<(usize, usize)>,
    /// Answers HTTP 500 to this many `getblock <hash> 3` calls, which only
    /// finalize makes.
    pub fail_prevout_blocks: usize,
    /// Delays every `getblock` reply.
    pub getblock_delay: Duration,
}

/// A fault-injecting JSON-RPC forwarder in front of a bitcoind. It also
/// answers `getblock <hash> 3`, which mockcore lacks, with each input's
/// prevout value.
pub struct Shim {
    pub url: String,
    state: Arc<ShimState>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Shim {
    /// Failures still to inject: `getblock`, `getblockchaininfo`, then
    /// `getblock <hash> 3`.
    pub fn pending_failures(&self) -> (usize, usize, usize) {
        let faults = self.state.faults.lock().unwrap();
        (
            faults.fail_getblock.map_or(0, |(_, n)| n),
            faults.fail_blockchaininfo.map_or(0, |(_, n)| n),
            faults.fail_prevout_blocks,
        )
    }

    /// Number of `getblock` verbosity 3 calls answered.
    pub fn prevout_blocks(&self) -> usize {
        *self.state.prevout_blocks.lock().unwrap()
    }
}

struct ShimState {
    upstream: String,
    client: reqwest::Client,
    faults: Mutex<Faults>,
    prevout_blocks: Mutex<usize>,
}

impl ShimState {
    async fn upstream(&self, method: &str, params: Value) -> Value {
        let reply = self
            .client
            .post(&self.upstream)
            .basic_auth("user", Some("pass"))
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert!(reply["error"].is_null(), "{method}: {reply}");
        reply["result"].clone()
    }

    /// `getblock <hash> 3`: transactions with each input's prevout value.
    async fn block_with_prevouts(&self, hash: &Value) -> Value {
        let raw = self.upstream("getblock", json!([hash, 0])).await;
        let block: Block = deserialize_hex(raw.as_str().unwrap()).unwrap();
        let mut txs = Vec::new();
        for tx in &block.txdata {
            let mut vin = Vec::new();
            for input in &tx.input {
                if input.previous_output.is_null() {
                    vin.push(json!({"coinbase": "00"}));
                    continue;
                }
                let raw = self
                    .upstream(
                        "getrawtransaction",
                        json!([input.previous_output.txid.to_string(), false]),
                    )
                    .await;
                let prev: Transaction = deserialize_hex(raw.as_str().unwrap()).unwrap();
                let value = prev.output[input.previous_output.vout as usize].value;
                vin.push(json!({"prevout": {"value": value.to_btc()}}));
            }
            txs.push(json!({"txid": tx.compute_txid().to_string(), "vin": vin}));
        }
        json!({"hash": hash, "tx": txs})
    }
}

impl Shim {
    pub fn start(upstream: String, faults: Faults) -> Shim {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(ShimState {
            upstream,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            faults: Mutex::new(faults),
            prevout_blocks: Mutex::default(),
        });
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(shim_handle)
            .with_state(state.clone());
        runtime.spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Shim {
            url,
            state,
            runtime: Some(runtime),
        }
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn injected_fault(request: &Value) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "result": null,
            "error": {"code": -1, "message": "injected fault"},
            "id": request["id"],
        })
        .to_string(),
    )
        .into_response()
}

async fn shim_handle(State(state): State<Arc<ShimState>>, body: Bytes) -> Response {
    let request = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    if request["method"] == "getblock" && request["params"][1] == 3 {
        let fail = {
            let mut faults = state.faults.lock().unwrap();
            let fail = faults.fail_prevout_blocks > 0;
            faults.fail_prevout_blocks = faults.fail_prevout_blocks.saturating_sub(1);
            fail
        };
        if fail {
            return injected_fault(&request);
        }
        *state.prevout_blocks.lock().unwrap() += 1;
        let block = state.block_with_prevouts(&request["params"][0]).await;
        return (
            [(header::CONTENT_TYPE, "application/json")],
            json!({"result": block, "error": null, "id": request["id"]}).to_string(),
        )
            .into_response();
    }
    let (fail, delay) = {
        let mut faults = state.faults.lock().unwrap();
        match request["method"].as_str() {
            Some("getblock") => {
                let fail = match &mut faults.fail_getblock {
                    Some((hash, remaining))
                        if *remaining > 0 && request["params"][0] == hash.to_string() =>
                    {
                        *remaining -= 1;
                        true
                    }
                    _ => false,
                };
                (fail, faults.getblock_delay)
            }
            Some("getblockchaininfo") => {
                let fail = match &mut faults.fail_blockchaininfo {
                    Some((skip, _)) if *skip > 0 => {
                        *skip -= 1;
                        false
                    }
                    Some((_, remaining)) if *remaining > 0 => {
                        *remaining -= 1;
                        true
                    }
                    _ => false,
                };
                (fail, Duration::ZERO)
            }
            _ => (false, Duration::ZERO),
        }
    };
    if fail {
        return injected_fault(&request);
    }
    tokio::time::sleep(delay).await;
    let response = state
        .client
        .post(&state.upstream)
        .basic_auth("user", Some("pass"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
    let reply = response.bytes().await.unwrap();
    (status, [(header::CONTENT_TYPE, "application/json")], reply).into_response()
}
