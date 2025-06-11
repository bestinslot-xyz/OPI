use std::error::Error;

use serde::Deserialize;

use crate::types::Transfer;

pub struct OpiDatabase {
    pub url: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct LatestHeightResponse {
    jsonrpc: String,
    id: i32,
    result: i32,
}

#[derive(Deserialize)]
struct HashAndTsResponse {
    jsonrpc: String,
    id: i32,
    result: BlockHashAndTsResult,
}

#[derive(Deserialize)]
struct BlockHashAndTsResult {
    block_hash: String,
    timestamp: i64,
}

#[derive(Deserialize)]
struct BRC20TxsResponse {
    jsonrpc: String,
    id: i32,
    result: Vec<BRC20TxResultItem>,
}

#[derive(Deserialize)]
struct BRC20TxResultItem {
    tx_id: String,
    inscription_id: String,
    inscription_number: i32,
    old_satpoint: Option<String>,
    new_satpoint: String,
    txid: String,
    new_pkscript: String,
    new_wallet: String,
    sent_as_fee: bool,
    content_hex: String,
    byte_len: u32,
    content_type_hex: String,
    parent_id: Option<String>,
}

impl OpiDatabase {
    pub fn new(
        url: String,
    ) -> Self {
        let client = reqwest::Client::new();
        OpiDatabase { url, client }
    }

    pub async fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        method: &str,
        params: &[serde_json::Value],
    ) -> Result<T, Box<dyn Error>> {
        let res = self.client
            .post(&url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await?
            .json::<T>()
            .await?;

        Ok(res)
    }

    pub async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        let res: LatestHeightResponse = self.rpc_call(self.url.clone(), "getLatestBlockHeight", &[]).await?;
        Ok(res.result)
    }

    pub async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>> {
        let res: HashAndTsResponse = self.rpc_call(self.url.clone(), "getBlockHashAndTs", &[serde_json::Value::Number(block_height.into())]).await?;
        Ok(res.result.block_hash)
    }

    pub async fn get_block_hash_and_time(
        &self,
        block_height: i32,
    ) -> Result<(String, i64), Box<dyn Error>> {
        let res: HashAndTsResponse = self.rpc_call(self.url.clone(), "getBlockHashAndTs", &[serde_json::Value::Number(block_height.into())]).await?;
        Ok((res.result.block_hash, res.result.timestamp))
    }

    pub async fn get_transfers(&self, block_height: i32) -> Result<Vec<Transfer>, Box<dyn Error>> {
        let res: BRC20TxsResponse = self.rpc_call(self.url.clone(), "getBlockBRC20Txes", &[serde_json::Value::Number(block_height.into())]).await?;
        let transfers: Vec<Transfer> = res.result.iter().map(|item| {
            Transfer {
                tx_id: item.tx_id.clone(),
                inscription_id: item.inscription_id.clone(),
                inscription_number: item.inscription_number,
                old_satpoint: item.old_satpoint.clone(),
                new_satpoint: item.new_satpoint.clone(),
                txid: item.txid.clone(),
                new_pkscript: item.new_pkscript.clone(),
                new_wallet: none_if_empty(Some(item.new_wallet.clone())),
                sent_as_fee: item.sent_as_fee,
                content: Some(serde_json::from_slice(&hex::decode(&item.content_hex).unwrap()).unwrap_or_default()),
                byte_length: item.byte_len as i32,
                content_type: item.content_type_hex.clone(),
                parent_inscription_id: item.parent_id.clone(),
            }
        }).collect();
        Ok(transfers)
    }
}

fn none_if_empty(s: Option<String>) -> Option<String> {
    if let Some(s) = s {
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}
