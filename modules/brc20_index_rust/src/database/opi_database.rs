use std::error::Error;

use db_reader::{BRC20Tx, Brc20ApiClient};
use jsonrpsee::http_client::HttpClient;

pub struct OpiDatabase {
    client: HttpClient,
}

impl OpiDatabase {
    pub fn new(url: String) -> Self {
        OpiDatabase {
            client: HttpClient::builder()
                .max_response_size(100 * 1024 * 1024) // 100 MB
                .build(url)
                .expect("Failed to create HTTP client"),
        }
    }

    pub async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        self.client
            .get_latest_block_height()
            .await?
            .map(|height| height as i32)
            .ok_or("Failed to get current block height".into())
    }

    pub async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>> {
        self.client
            .get_block_hash_and_ts(block_height as u32)
            .await?
            .map(|res| res.block_hash)
            .ok_or("Block hash not found".into())
    }

    pub async fn get_block_hash_and_time(
        &self,
        block_height: i32,
    ) -> Result<(String, i64), Box<dyn Error>> {
        self.client
            .get_block_hash_and_ts(block_height as u32)
            .await?
            .map(|res| (res.block_hash, res.timestamp as i64))
            .ok_or("Block hash and time not found".into())
    }

    pub async fn get_transfers(&self, block_height: i32) -> Result<Vec<BRC20Tx>, Box<dyn Error>> {
        self.client
            .get_block_brc20_txes(block_height as u32)
            .await?
            .ok_or("No transfers found".into())
    }
}
