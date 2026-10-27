//! Minimal bitcoind JSON-RPC client for the driver's own calls, which bypass
//! the gate proxy.

use {
    crate::proxy::{UpstreamAuth, credentials},
    anyhow::{Context, Result, anyhow},
    bitcoin::{Amount, Block, BlockHash, block::Header, consensus::encode::deserialize_hex},
    serde_json::{Value, json},
};

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

    async fn hex(&self, method: &str, params: Value) -> Result<String> {
        let result = self.call(method, params).await?;
        result
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("{method} returned {result}"))
    }
}
