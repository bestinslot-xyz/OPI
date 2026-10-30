//! Minimal bitcoind JSON-RPC client for the driver's own calls, which bypass
//! the gate proxy.

use {
    crate::proxy::{UpstreamAuth, credentials},
    anyhow::{Context, Result, anyhow},
    bitcoin::{
        Amount, Block, BlockHash, Transaction, Txid, block::Header,
        consensus::encode::deserialize_hex,
    },
    serde_json::{Value, json},
};

/// Transactions read per JSON-RPC batch. bitcoind answers a batch on one of
/// its RPC threads, so reads in batches leave its work queue to ord.
const TX_BATCH: usize = 500;

/// Longest a call may take before it fails and its caller retries; a
/// `getblock` verbosity 3 of a full block is the slowest.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub(crate) struct BitcoinRpc {
    url: String,
    auth: UpstreamAuth,
    client: reqwest::Client,
}

impl BitcoinRpc {
    pub(crate) fn new(url: String, auth: UpstreamAuth) -> Result<BitcoinRpc> {
        Ok(BitcoinRpc {
            url,
            auth,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(CALL_TIMEOUT)
                .build()
                .context("building bitcoind client")?,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let (user, pass) = credentials(&self.auth).await?;
        let reply = self
            .client
            .post(&self.url)
            .basic_auth(user, Some(pass))
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .with_context(|| format!("calling bitcoind {method}"))?
            .json::<Value>()
            .await
            .with_context(|| format!("reading bitcoind {method} reply"))?;
        if !reply["error"].is_null() {
            return Err(anyhow!("bitcoind {method} failed: {}", reply["error"]));
        }
        Ok(reply["result"].clone())
    }

    /// Height of bitcoind's best block.
    pub(crate) async fn block_count(&self) -> Result<u32> {
        let count = self.call("getblockcount", json!([])).await?;
        let count = count
            .as_u64()
            .with_context(|| format!("getblockcount returned {count}"))?;
        Ok(u32::try_from(count)?)
    }

    pub(crate) async fn block(&self, hash: BlockHash) -> Result<Block> {
        let hex = self.hex("getblock", json!([hash.to_string(), 0])).await?;
        Ok(deserialize_hex(&hex)?)
    }

    pub(crate) async fn header(&self, hash: BlockHash) -> Result<Header> {
        let hex = self
            .hex("getblockheader", json!([hash.to_string(), false]))
            .await?;
        Ok(deserialize_hex(&hex)?)
    }

    /// The value of every input of every transaction of block `hash`, from
    /// `getblock` verbosity 3 prevouts. The coinbase has none.
    pub(crate) async fn input_values(&self, hash: BlockHash) -> Result<Vec<Vec<u64>>> {
        let block = self.call("getblock", json!([hash.to_string(), 3])).await?;
        let txs = block["tx"]
            .as_array()
            .with_context(|| format!("getblock {hash} 3 returned no transactions"))?;
        txs.iter()
            .map(|tx| {
                let inputs = tx["vin"]
                    .as_array()
                    .with_context(|| format!("getblock {hash} 3 returned a tx without vin"))?;
                inputs
                    .iter()
                    .filter(|input| input.get("coinbase").is_none())
                    .map(|input| {
                        let btc = input["prevout"]["value"].as_f64().with_context(|| {
                            format!(
                                "getblock {hash} 3 returned an input without prevout; \
                                 bitcoind needs the block's undo data"
                            )
                        })?;
                        Ok(Amount::from_btc(btc)?.to_sat())
                    })
                    .collect()
            })
            .collect()
    }

    /// The transactions with `txids`, in that order, from bitcoind's
    /// transaction index, in JSON-RPC batches of [`TX_BATCH`].
    pub(crate) async fn transactions(&self, txids: &[Txid]) -> Result<Vec<Transaction>> {
        let mut transactions = Vec::with_capacity(txids.len());
        for batch in txids.chunks(TX_BATCH) {
            let requests = batch
                .iter()
                .enumerate()
                .map(|(id, txid)| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "getrawtransaction",
                        "params": [txid.to_string(), false],
                    })
                })
                .collect::<Vec<_>>();
            let (user, pass) = credentials(&self.auth).await?;
            let replies = self
                .client
                .post(&self.url)
                .basic_auth(user, Some(pass))
                .json(&requests)
                .send()
                .await
                .context("calling bitcoind getrawtransaction")?
                .json::<Vec<Value>>()
                .await
                .context("reading bitcoind getrawtransaction replies")?;
            let mut hexes = vec![None; batch.len()];
            for reply in replies {
                let id = reply["id"]
                    .as_u64()
                    .and_then(|id| usize::try_from(id).ok())
                    .filter(|id| *id < batch.len())
                    .with_context(|| format!("getrawtransaction reply with id {}", reply["id"]))?;
                if !reply["error"].is_null() {
                    return Err(anyhow!(
                        "bitcoind getrawtransaction {} failed: {}",
                        batch[id],
                        reply["error"]
                    ));
                }
                hexes[id] = reply["result"].as_str().map(str::to_owned);
            }
            for (txid, hex) in batch.iter().zip(hexes) {
                let hex = hex.with_context(|| format!("no transaction {txid} in the reply"))?;
                transactions.push(deserialize_hex(&hex)?);
            }
        }
        Ok(transactions)
    }

    async fn hex(&self, method: &str, params: Value) -> Result<String> {
        let result = self.call(method, params).await?;
        result
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("{method} returned {result}"))
    }
}
