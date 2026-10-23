use std::error::Error;

use db_reader::BRC20Tx;

/// Block-level reads the BRC-20 indexer needs from its main data source.
#[async_trait::async_trait]
pub trait Brc20Source: Send + Sync {
    async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>>;
    async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>>;
    async fn get_block_hash_and_time(
        &self,
        block_height: i32,
    ) -> Result<(String, i64), Box<dyn Error>>;
    async fn get_transfers(&self, block_height: i32) -> Result<Vec<BRC20Tx>, Box<dyn Error>>;
}
