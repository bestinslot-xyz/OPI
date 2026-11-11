//! The UTXO set as an opi module: every unspent output, keyed by its outpoint
//! and indexed by its script, in the `db_init.sql` schema the API reads.
//!
//! The journal carries no outputs, so the module reads each block from
//! bitcoind by the hash the journal gives it, and stays on the journal's
//! chain. Far below the journal's tip it reads blocks by height and writes
//! many per transaction, keeping no undo data; within `MAX_ROLLBACK_DEPTH` of
//! the tip it writes one block per transaction with the undo data a rollback
//! needs, and prunes that data once it falls out of the window.

mod db;
mod delta;

use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use bitcoin::{Block, BlockHash};
use futures::{StreamExt, TryStreamExt};
use opi_core::{BlockRef, BlockSource, BlockView, MAX_ROLLBACK_DEPTH, Module};

pub use db::DB_VERSION;
pub use delta::{Delta, Output, Spend, txid_bytes};

use db::{Db, Undo};

/// Most outputs and spends one catch-up transaction writes.
const CATCH_UP_BATCH: usize = 250_000;
/// Blocks a catch-up fetches from bitcoind ahead of the one it folds in.
const FETCH_AHEAD: usize = 16;
/// Applied heights between two prunes of the undo data.
const PRUNE_EVERY: u32 = 100;

#[derive(Debug, Clone)]
pub struct UtxosConfig {
    pub db_user: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_database: String,
    pub db_password: Option<String>,
    pub db_ssl: bool,
    pub network_type: String,
}

impl UtxosConfig {
    /// Reads `UTXOS_DB_*` and `NETWORK_TYPE`.
    pub fn from_env() -> anyhow::Result<UtxosConfig> {
        UtxosConfig::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Reads the variables of `from_env` through `get`.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<UtxosConfig> {
        let var = |key: &str| get(key).filter(|value| !value.is_empty());
        let network_type = var("NETWORK_TYPE").unwrap_or_else(|| "mainnet".into());
        if !["mainnet", "testnet", "testnet4", "signet", "regtest"].contains(&network_type.as_str())
        {
            bail!("unknown NETWORK_TYPE {network_type}");
        }
        Ok(UtxosConfig {
            db_user: var("UTXOS_DB_USER").unwrap_or_else(|| "postgres".into()),
            db_host: var("UTXOS_DB_HOST").unwrap_or_else(|| "localhost".into()),
            db_port: var("UTXOS_DB_PORT")
                .map(|port| port.parse().context("UTXOS_DB_PORT"))
                .transpose()?
                .unwrap_or(5432),
            db_database: var("UTXOS_DB_DATABASE").unwrap_or_else(|| "postgres".into()),
            db_password: var("UTXOS_DB_PASSWD"),
            db_ssl: var("UTXOS_DB_SSL").is_some_and(|ssl| ssl == "true"),
            network_type,
        })
    }
}

/// Raw blocks from bitcoind.
#[async_trait::async_trait]
pub trait BlockFetcher: Send + Sync {
    /// The hash of the block at `height` on bitcoind's best chain.
    async fn block_hash(&self, height: u32) -> anyhow::Result<BlockHash>;
    async fn block(&self, hash: BlockHash) -> anyhow::Result<Block>;
}

pub struct UtxosModule {
    db: Db,
    blocks: Arc<dyn BlockFetcher>,
    journal: Arc<dyn BlockSource>,
}

impl UtxosModule {
    /// Connects and prepares the schema. `blocks` supplies the blocks and
    /// `journal` the tip that decides between catching up and following.
    pub async fn new(
        config: UtxosConfig,
        blocks: Arc<dyn BlockFetcher>,
        journal: Arc<dyn BlockSource>,
    ) -> anyhow::Result<UtxosModule> {
        let db = Db::connect(&config).await?;
        Ok(UtxosModule {
            db,
            blocks,
            journal,
        })
    }

    /// The highest height a catch-up writes: `MAX_ROLLBACK_DEPTH - 1` below
    /// the journal's tip, a height every journal still holds, so the
    /// scheduler can check the module's cursor against it. `None` while the
    /// journal is shorter than that.
    fn catch_up_target(&self) -> anyhow::Result<Option<u32>> {
        Ok(self
            .journal
            .tip()?
            .and_then(|tip| tip.checked_sub(MAX_ROLLBACK_DEPTH - 1)))
    }

    /// Writes every block from the cursor up to the catch-up target, reading
    /// blocks from bitcoind by height and keeping no undo data, until the
    /// cursor reaches the target as it moves. Returns the blocks written.
    pub async fn catch_up(&mut self) -> anyhow::Result<u32> {
        let mut written = 0;
        while let Some(target) = self.catch_up_target()? {
            let cursor = self.db.cursor().await?;
            let next = cursor.as_ref().map_or(0, |cursor| cursor.height + 1);
            if next > target {
                break;
            }
            let previous = cursor
                .map(|cursor| {
                    cursor
                        .hash
                        .parse::<BlockHash>()
                        .with_context(|| format!("parsing stored block hash {}", cursor.hash))
                })
                .transpose()?;
            written += self.catch_up_range(next, target, previous).await?;
        }
        Ok(written)
    }

    /// Writes blocks `from..=to` from bitcoind. Each block must extend the
    /// one before it, the first one the stored `previous`.
    async fn catch_up_range(
        &mut self,
        from: u32,
        to: u32,
        mut previous: Option<BlockHash>,
    ) -> anyhow::Result<u32> {
        let blocks = self.blocks.clone();
        let mut fetched = futures::stream::iter(from..=to)
            .map(|height| {
                let blocks = blocks.clone();
                async move {
                    let hash = blocks
                        .block_hash(height)
                        .await
                        .with_context(|| format!("reading the hash of block {height}"))?;
                    let block = blocks
                        .block(hash)
                        .await
                        .with_context(|| format!("reading block {height}"))?;
                    anyhow::Ok((height, hash, block))
                }
            })
            .buffered(FETCH_AHEAD);
        let mut delta = Delta::new();
        let mut batch = Vec::new();
        let mut written = 0;
        while let Some((height, hash, block)) = fetched.try_next().await? {
            check_hash(&block, hash, height)?;
            if let Some(previous) = previous
                && block.header.prev_blockhash != previous
            {
                bail!(
                    "bitcoind's block {hash} at height {height} does not extend block {previous}; \
                     the chain changed under the catch-up, which restarts"
                );
            }
            previous = Some(hash);
            delta.add_block(height, &block);
            batch.push((height, hash.to_string()));
            if delta.len() >= CATCH_UP_BATCH || height == to {
                self.db
                    .write_blocks(&batch, std::mem::take(&mut delta), Undo::Skip)
                    .await?;
                written += batch.len() as u32;
                tracing::info!(height, target = to, "utxos caught up to block {height}");
                batch.clear();
            }
        }
        Ok(written)
    }
}

/// Fails unless `block` hashes to `hash`, the hash it was asked for.
fn check_hash(block: &Block, hash: BlockHash, height: u32) -> anyhow::Result<()> {
    let got = block.block_hash();
    if got != hash {
        return Err(anyhow!(
            "bitcoind returned block {got} for {hash} at height {height}"
        ));
    }
    Ok(())
}

#[async_trait::async_trait]
impl Module for UtxosModule {
    fn name(&self) -> &'static str {
        "utxos"
    }

    fn pool_stats(&self) -> Option<opi_core::metrics::PoolStats> {
        Some(self.db.pool_stats())
    }

    fn start_height(&self) -> u32 {
        0
    }

    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>> {
        self.db.cursor().await
    }

    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>> {
        self.db.block_hash(height).await
    }

    async fn rollback_to(&mut self, height: u32) -> anyhow::Result<()> {
        self.db.rollback_to(height).await
    }

    async fn prune_below(&mut self, height: u32) -> anyhow::Result<()> {
        self.db.prune_below(height).await
    }

    /// Catches up when the block is at or below the catch-up target, which
    /// writes it and the blocks after it; otherwise reads the block by the
    /// journal's hash and writes it with its undo data. Every
    /// `PRUNE_EVERY` heights it drops the undo data that fell out of the
    /// rollback window.
    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        if self
            .catch_up_target()?
            .is_some_and(|target| block.height <= target)
        {
            let hash = self.blocks.block_hash(block.height).await?;
            if hash.to_string() != block.hash {
                bail!(
                    "bitcoind's block {hash} at height {} is not the journal's {}",
                    block.height,
                    block.hash
                );
            }
            self.catch_up().await?;
            return Ok(());
        }
        let hash: BlockHash = block
            .hash
            .parse()
            .with_context(|| format!("parsing block hash {}", block.hash))?;
        let raw = self
            .blocks
            .block(hash)
            .await
            .with_context(|| format!("reading block {}", block.height))?;
        check_hash(&raw, hash, block.height)?;
        let mut delta = Delta::new();
        delta.add_block(block.height, &raw);
        self.db
            .write_blocks(&[(block.height, block.hash.clone())], delta, Undo::Keep)
            .await?;
        if block.height.is_multiple_of(PRUNE_EVERY) {
            let below = (block.height + 1).saturating_sub(MAX_ROLLBACK_DEPTH);
            if let Err(err) = self.db.prune_below(below).await {
                tracing::warn!(below, "pruning the utxos undo data failed: {err:#}");
            }
        }
        Ok(())
    }
}
