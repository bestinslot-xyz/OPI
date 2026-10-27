//! The bitmap metaprotocol as an opi module. It writes the schema in
//! `db_init.sql` and the cumulative event hashes that the bitmap API (`opi-api`) and the OPI
//! network read.

mod db;
mod report;
mod rules;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use opi_core::{BlockRef, BlockSource, BlockView, Module};

pub use report::{DB_VERSION, INDEXER_VERSION};
pub use rules::{
    Candidate, bitmap_number, block_event_hash, candidates, cumulative_hash, event_str,
    is_valid_bitmap, sha256_hex,
};

use db::{BlockHashes, Db};

const REPORT_URL_DEFAULT: &str = "https://api.opi.network/report_block";
const REPORT_NAME_DEFAULT: &str = "opi_bitmap_indexer";
const REPORT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct BitmapConfig {
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

impl BitmapConfig {
    /// Reads `BITMAP_DB_*`, `NETWORK_TYPE` and the shared `REPORT_*` variables.
    pub fn from_env() -> anyhow::Result<BitmapConfig> {
        BitmapConfig::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Reads the variables of `from_env` through `get`.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<BitmapConfig> {
        let var = |key: &str| get(key).filter(|value| !value.is_empty());
        let network_type = var("NETWORK_TYPE").unwrap_or_else(|| "mainnet".into());
        first_inscription_height(&network_type)?;
        Ok(BitmapConfig {
            db_user: var("BITMAP_DB_USER").unwrap_or_else(|| "postgres".into()),
            db_host: var("BITMAP_DB_HOST").unwrap_or_else(|| "localhost".into()),
            db_port: var("BITMAP_DB_PORT")
                .map(|port| port.parse().context("BITMAP_DB_PORT"))
                .transpose()?
                .unwrap_or(5432),
            db_database: var("BITMAP_DB_DATABASE").unwrap_or_else(|| "postgres".into()),
            db_password: var("BITMAP_DB_PASSWD"),
            db_ssl: var("BITMAP_DB_SSL").is_some_and(|ssl| ssl == "true"),
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

/// First height with inscriptions on `network`.
fn first_inscription_height(network: &str) -> anyhow::Result<u32> {
    match network {
        "mainnet" => Ok(767_430),
        "testnet" => Ok(2_413_343),
        "testnet4" | "regtest" => Ok(0),
        "signet" => Ok(112_402),
        other => Err(anyhow!("unknown NETWORK_TYPE {other}")),
    }
}

pub struct BitmapModule {
    config: BitmapConfig,
    db: Db,
    journal: Arc<dyn BlockSource>,
    http: reqwest::Client,
    start_height: u32,
    last_report_height: u32,
}

impl BitmapModule {
    /// Connects, prepares the schema and drops rows left above the cursor.
    /// `journal` supplies the tip that the reporting cadence reads.
    pub async fn new(
        config: BitmapConfig,
        journal: Arc<dyn BlockSource>,
    ) -> anyhow::Result<BitmapModule> {
        let start_height = first_inscription_height(&config.network_type)?;
        let db = Db::connect(&config).await?;
        let mut module = BitmapModule {
            config,
            db,
            journal,
            http: reqwest::Client::builder().timeout(REPORT_TIMEOUT).build()?,
            start_height,
            last_report_height: 0,
        };
        module.drop_residue().await?;
        Ok(module)
    }

    async fn drop_residue(&mut self) -> anyhow::Result<()> {
        let next = match self.db.cursor().await? {
            Some(cursor) => cursor.height + 1,
            None => self.start_height,
        };
        if self
            .db
            .max_bitmap_height()
            .await?
            .is_some_and(|height| height >= next)
        {
            tracing::warn!("bitmap rows above height {} dropped", next.wrapping_sub(1));
            self.db.rollback_to(next.wrapping_sub(1)).await?;
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
impl Module for BitmapModule {
    fn name(&self) -> &'static str {
        "bitmap"
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
        self.db.rollback_to(height).await
    }

    /// Writes the block, then reports it. The block is committed before the
    /// report, so a report failure is logged and never fails the block.
    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        let found = candidates(block)?;
        let hashes = self
            .db
            .write_block(block.height, &block.hash, &found)
            .await?;
        if self.reporting_enabled()
            && let Err(error) = self.report(block, &hashes).await
        {
            tracing::error!(
                "bitmap report for block {} skipped: {error:#}",
                block.height
            );
        }
        Ok(())
    }
}
