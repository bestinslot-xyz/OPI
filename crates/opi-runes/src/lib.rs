//! The runes metaprotocol as an opi module. It writes the journal's rune feed
//! into the schema in `db_init.sql`, which the runes API (`opi-api`) reads, and reports the
//! cumulative event hashes to the OPI network.

mod db;
pub mod holders;
pub mod names;
mod report;
mod rules;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use opi_core::{BlockRef, BlockSource, BlockView, Module};

pub use report::{DB_VERSION, EVENT_HASH_VERSION, INDEXER_VERSION};
pub use rules::{
    Held, block_event_hash, complete_events, counters, cumulative_hash, event_str, sha256_hex,
    symbol_hex, wallet_addr,
};

use db::{BlockHashes, Db};

const REPORT_URL_DEFAULT: &str = "https://api.opi.network/report_block";
const REPORT_NAME_DEFAULT: &str = "opi_runes_indexer";
const REPORT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RunesConfig {
    pub db_user: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_database: String,
    pub db_password: Option<String>,
    pub db_ssl: bool,
    pub network_type: String,
    pub report_to_indexer: bool,
    pub report_url: String,
    pub report_retries: u32,
    pub report_name: String,
}

impl RunesConfig {
    /// Reads `RUNES_DB_*`, `NETWORK_TYPE` and the shared `REPORT_*` variables.
    pub fn from_env() -> anyhow::Result<RunesConfig> {
        RunesConfig::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Reads the variables of `from_env` through `get`. `RUNES_DB_DATABASE`
    /// is required and `NETWORK_TYPE` must name a known network.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<RunesConfig> {
        let var = |key: &str| get(key).filter(|value| !value.is_empty());
        let network_type = var("NETWORK_TYPE").unwrap_or_else(|| "mainnet".into());
        if !NETWORKS.contains(&network_type.as_str()) {
            return Err(anyhow!("unknown NETWORK_TYPE {network_type}"));
        }
        Ok(RunesConfig {
            db_user: var("RUNES_DB_USER").unwrap_or_else(|| "postgres".into()),
            db_host: var("RUNES_DB_HOST").unwrap_or_else(|| "localhost".into()),
            db_port: var("RUNES_DB_PORT")
                .map(|port| port.parse().context("RUNES_DB_PORT"))
                .transpose()?
                .unwrap_or(5432),
            db_database: var("RUNES_DB_DATABASE")
                .ok_or_else(|| anyhow!("RUNES_DB_DATABASE is not set"))?,
            db_password: var("RUNES_DB_PASSWD"),
            db_ssl: var("RUNES_DB_SSL").is_some_and(|ssl| ssl == "true"),
            network_type,
            report_to_indexer: var("REPORT_TO_INDEXER").is_none_or(|report| report == "true"),
            report_url: var("REPORT_URL").unwrap_or_else(|| REPORT_URL_DEFAULT.into()),
            report_retries: var("REPORT_RETRIES")
                .map(|retries| retries.parse().context("REPORT_RETRIES"))
                .transpose()?
                .unwrap_or(10),
            report_name: var("REPORT_NAME").unwrap_or_else(|| REPORT_NAME_DEFAULT.into()),
        })
    }
}

const NETWORKS: [&str; 5] = ["mainnet", "testnet", "testnet4", "signet", "regtest"];

/// First height with rune state on `network`, ord's first rune height.
pub fn start_height(network: &str) -> u32 {
    match network {
        "mainnet" => 840_000,
        "testnet" => 2_520_000,
        _ => 0,
    }
}

/// First height whose event hashes are stored and reported on `network`.
pub fn hash_start_height(network: &str) -> u32 {
    match network {
        "mainnet" => 840_000,
        "testnet" => 2_520_000,
        "signet" => 173_831,
        _ => 0,
    }
}

pub struct RunesModule {
    config: RunesConfig,
    db: Db,
    journal: Arc<dyn BlockSource>,
    http: reqwest::Client,
    start_height: u32,
    hash_start_height: u32,
    last_report_height: u32,
}

impl RunesModule {
    /// Connects, prepares the schema, drops rows left above the cursor and
    /// seeds mainnet's `1:0` entry. `journal` supplies the tip that the
    /// reporting cadence reads.
    pub async fn new(
        config: RunesConfig,
        journal: Arc<dyn BlockSource>,
    ) -> anyhow::Result<RunesModule> {
        if !NETWORKS.contains(&config.network_type.as_str()) {
            return Err(anyhow!("unknown NETWORK_TYPE {}", config.network_type));
        }
        let db = Db::connect(&config).await?;
        let mut module = RunesModule {
            start_height: start_height(&config.network_type),
            hash_start_height: hash_start_height(&config.network_type),
            config,
            db,
            journal,
            http: reqwest::Client::builder().timeout(REPORT_TIMEOUT).build()?,
            last_report_height: 0,
        };
        module.drop_residue().await?;
        if module.config.network_type == "mainnet" {
            module.db.ensure_mainnet_seed().await?;
        }
        Ok(module)
    }

    /// Drops rows at or above the next height to index: above the cursor, or
    /// every row when there is no cursor and the start height is 0.
    async fn drop_residue(&mut self) -> anyhow::Result<()> {
        let next = match self.db.cursor().await? {
            Some(cursor) => cursor.height + 1,
            None => self.start_height,
        };
        if self
            .db
            .max_row_height()
            .await?
            .is_some_and(|height| height >= next)
        {
            let keep = next.checked_sub(1);
            match keep {
                Some(keep) => tracing::warn!("runes rows above height {keep} dropped"),
                None => tracing::warn!("all runes rows dropped"),
            }
            self.db.rollback_to(keep).await?;
        }
        Ok(())
    }

    fn reporting_enabled(&self) -> bool {
        self.config.report_to_indexer && self.config.network_type != "regtest"
    }

    /// Reports the committed block when the cadence asks for it.
    async fn report(&mut self, block: &BlockView, hashes: &BlockHashes) -> anyhow::Result<()> {
        let tip = self.journal.tip()?.unwrap_or(block.height);
        if !report::should_report(block.height, tip, self.last_report_height) {
            return Ok(());
        }
        let body = report::payload(
            &self.config.report_name,
            &self.config.network_type,
            block.height,
            &block.hash,
            &hashes.block_event_hash,
            &hashes.cumulative_event_hash,
        );
        report::send_with_retries(
            &self.http,
            &self.config.report_url,
            self.config.report_retries,
            &body,
        )
        .await;
        self.last_report_height = block.height;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Module for RunesModule {
    fn name(&self) -> &'static str {
        "runes"
    }

    fn pool_stats(&self) -> Option<opi_core::metrics::PoolStats> {
        Some(self.db.pool_stats())
    }

    fn start_height(&self) -> u32 {
        self.start_height
    }

    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>> {
        self.db.cursor().await
    }

    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>> {
        self.db.block_hash(height).await
    }

    async fn rollback_to(&mut self, height: u32) -> anyhow::Result<()> {
        self.db.rollback_to(Some(height)).await
    }

    async fn prune_below(&mut self, height: u32) -> anyhow::Result<()> {
        self.db.prune_below(height).await
    }

    /// Writes the block, then reports it when its height is hashed. The block
    /// is committed before the report, so a report failure is logged and
    /// never fails the block.
    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        let hashed = block.height >= self.hash_start_height;
        let hashes = self
            .db
            .write_block(block, &self.config.network_type, hashed)
            .await?;
        if let Some(hashes) = hashes
            && self.reporting_enabled()
            && let Err(error) = self.report(block, &hashes).await
        {
            tracing::error!("runes report for block {} skipped: {error:#}", block.height);
        }
        Ok(())
    }
}
