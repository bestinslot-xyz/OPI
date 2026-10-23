//! Minimal bitcoind JSON-RPC client for the driver's own calls, which bypass
//! the gate proxy.

use {
    crate::proxy::{UpstreamAuth, credentials},
    anyhow::{Context, Result, anyhow},
    bitcoin::{Block, BlockHash, block::Header, consensus::encode::deserialize_hex},
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

    async fn hex(&self, method: &str, params: Value) -> Result<String> {
        let result = self.call(method, params).await?;
        result
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("{method} returned {result}"))
    }
}
