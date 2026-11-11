//! The bitcoind calls the mempool view makes, as JSON-RPC over HTTP.

use {
    super::Bitcoind,
    crate::config::{MempoolSettings, RpcAuth},
    anyhow::{Context, Result, anyhow},
    bitcoin::{Transaction, Txid, consensus::encode::deserialize_hex},
    serde_json::{Value, json},
    std::{collections::HashMap, time::Duration},
};

/// Longest one call may take; a batch of mempool entries is the slowest.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Rpc {
    url: String,
    auth: RpcAuth,
    client: reqwest::Client,
}

impl Rpc {
    pub fn new(settings: &MempoolSettings) -> Result<Rpc> {
        Ok(Rpc {
            url: settings.rpc_url.clone(),
            auth: settings.auth.clone(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(CALL_TIMEOUT)
                .build()
                .context("building the bitcoind client")?,
        })
    }

    async fn credentials(&self) -> Result<(String, String)> {
        match &self.auth {
            RpcAuth::UserPass(user, pass) => Ok((user.clone(), pass.clone())),
            RpcAuth::Cookie(path) => {
                let cookie = tokio::fs::read_to_string(path)
                    .await
                    .with_context(|| format!("reading bitcoind's cookie {}", path.display()))?;
                let (user, pass) = cookie
                    .trim()
                    .split_once(':')
                    .ok_or_else(|| anyhow!("bitcoind's cookie {} is malformed", path.display()))?;
                Ok((user.to_string(), pass.to_string()))
            }
        }
    }

    /// Sends `calls` as one batch and answers each call's result, in order:
    /// `Err` for a call bitcoind refused.
    async fn batch(&self, calls: &[(&str, Value)]) -> Result<Vec<Result<Value, String>>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let (user, pass) = self.credentials().await?;
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(id, (method, params))| {
                json!({"jsonrpc": "1.0", "id": id, "method": method, "params": params})
            })
            .collect();
        let replies: Vec<Value> = self
            .client
            .post(&self.url)
            .basic_auth(user, Some(pass))
            .json(&body)
            .send()
            .await
            .context("calling bitcoind")?
            .error_for_status()
            .context("calling bitcoind")?
            .json()
            .await
            .context("reading bitcoind's reply")?;
        let mut by_id: HashMap<u64, Value> = HashMap::new();
        for reply in replies {
            if let Some(id) = reply["id"].as_u64() {
                by_id.insert(id, reply);
            }
        }
        (0..calls.len() as u64)
            .map(|id| {
                let reply = by_id
                    .remove(&id)
                    .ok_or_else(|| anyhow!("bitcoind answered no reply {id}"))?;
                Ok(if reply["error"].is_null() {
                    Ok(reply["result"].clone())
                } else {
                    Err(reply["error"].to_string())
                })
            })
            .collect()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.batch(&[(method, params)])
            .await?
            .pop()
            .expect("one call answers one reply")
            .map_err(|error| anyhow!("bitcoind {method} failed: {error}"))
    }
}

#[async_trait::async_trait]
impl Bitcoind for Rpc {
    async fn mempool_txids(&self) -> Result<Vec<Txid>> {
        let txids = self.call("getrawmempool", json!([false])).await?;
        txids
            .as_array()
            .ok_or_else(|| anyhow!("getrawmempool returned {txids}"))?
            .iter()
            .map(|txid| {
                txid.as_str()
                    .and_then(|txid| txid.parse().ok())
                    .ok_or_else(|| anyhow!("getrawmempool returned txid {txid}"))
            })
            .collect()
    }

    async fn block_count(&self) -> Result<u32> {
        let count = self.call("getblockcount", json!([])).await?;
        Ok(u32::try_from(count.as_u64().ok_or_else(|| {
            anyhow!("getblockcount returned {count}")
        })?)?)
    }

    async fn entries(&self, txids: &[Txid]) -> Result<Vec<Option<(Transaction, u64, u64)>>> {
        let calls: Vec<(&str, Value)> = txids
            .iter()
            .flat_map(|txid| {
                [
                    ("getrawtransaction", json!([txid.to_string(), false])),
                    ("getmempoolentry", json!([txid.to_string()])),
                ]
            })
            .collect();
        let replies = self.batch(&calls).await?;
        replies
            .chunks(2)
            .map(|pair| {
                // a transaction that left between the listing and this call
                // answers an error, and is skipped
                let (Ok(hex), Ok(entry)) = (&pair[0], &pair[1]) else {
                    return Ok(None);
                };
                let hex = hex
                    .as_str()
                    .ok_or_else(|| anyhow!("getrawtransaction returned {hex}"))?;
                let tx: Transaction = deserialize_hex(hex)?;
                let fee = entry["fees"]["base"]
                    .as_f64()
                    .ok_or_else(|| anyhow!("getmempoolentry returned {entry}"))?;
                let vsize = entry["vsize"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("getmempoolentry returned {entry}"))?;
                Ok(Some((tx, (fee * 1e8).round() as u64, vsize)))
            })
            .collect()
    }
}
