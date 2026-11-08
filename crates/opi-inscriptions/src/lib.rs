//! Inscription metadata as an opi module: the current location, owner and
//! content type of every bound inscription, written from the journal's
//! inscription moves into the schema in `db_init.sql`, which the
//! inscriptions API (`opi-api`) reads.

mod db;
mod rules;

use anyhow::{Context, anyhow};
use opi_core::{BlockRef, BlockView, Module};

pub use rules::{address, content_type_text, media_type};

use db::Db;

pub const INDEXER_VERSION: &str = "opi-inscriptions v1.0.0";
pub const DB_VERSION: i32 = 1;

const NETWORKS: [&str; 5] = ["mainnet", "testnet", "testnet4", "signet", "regtest"];

#[derive(Debug, Clone)]
pub struct InscriptionsConfig {
    pub db_user: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_database: String,
    pub db_password: Option<String>,
    pub db_ssl: bool,
    pub network_type: String,
}

impl InscriptionsConfig {
    /// Reads `INSCRIPTIONS_DB_*` and `NETWORK_TYPE`.
    pub fn from_env() -> anyhow::Result<InscriptionsConfig> {
        InscriptionsConfig::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Reads the variables of `from_env` through `get`.
    /// `INSCRIPTIONS_DB_DATABASE` is required and `NETWORK_TYPE` must name a
    /// known network.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<InscriptionsConfig> {
        let var = |key: &str| get(key).filter(|value| !value.is_empty());
        let network_type = var("NETWORK_TYPE").unwrap_or_else(|| "mainnet".into());
        if !NETWORKS.contains(&network_type.as_str()) {
            return Err(anyhow!("unknown NETWORK_TYPE {network_type}"));
        }
        Ok(InscriptionsConfig {
            db_user: var("INSCRIPTIONS_DB_USER").unwrap_or_else(|| "postgres".into()),
            db_host: var("INSCRIPTIONS_DB_HOST").unwrap_or_else(|| "localhost".into()),
            db_port: var("INSCRIPTIONS_DB_PORT")
                .map(|port| port.parse().context("INSCRIPTIONS_DB_PORT"))
                .transpose()?
                .unwrap_or(5432),
            db_database: var("INSCRIPTIONS_DB_DATABASE")
                .ok_or_else(|| anyhow!("INSCRIPTIONS_DB_DATABASE is not set"))?,
            db_password: var("INSCRIPTIONS_DB_PASSWD"),
            db_ssl: var("INSCRIPTIONS_DB_SSL").is_some_and(|ssl| ssl == "true"),
            network_type,
        })
    }
}

/// First height with inscriptions on `network`, ord's first inscription
/// height.
pub fn start_height(network: &str) -> u32 {
    match network {
        "mainnet" => 767_430,
        "signet" => 112_402,
        "testnet" => 2_413_343,
        _ => 0,
    }
}

pub struct InscriptionsModule {
    config: InscriptionsConfig,
    db: Db,
    start_height: u32,
}

impl InscriptionsModule {
    /// Connects, prepares the schema and drops rows left above the cursor.
    pub async fn new(config: InscriptionsConfig) -> anyhow::Result<InscriptionsModule> {
        if !NETWORKS.contains(&config.network_type.as_str()) {
            return Err(anyhow!("unknown NETWORK_TYPE {}", config.network_type));
        }
        let db = Db::connect(&config).await?;
        let module = InscriptionsModule {
            start_height: start_height(&config.network_type),
            config,
            db,
        };
        module.drop_residue().await?;
        Ok(module)
    }

    /// Drops rows at or above the next height to index: above the cursor, or
    /// every row when there is no cursor.
    async fn drop_residue(&self) -> anyhow::Result<()> {
        let keep = self.db.cursor().await?.map(|cursor| cursor.height);
        let next = keep.map_or(self.start_height, |height| height + 1);
        if self
            .db
            .max_row_height()
            .await?
            .is_some_and(|height| height >= next)
        {
            match keep {
                Some(keep) => tracing::warn!("inscriptions rows above height {keep} dropped"),
                None => tracing::warn!("all inscriptions rows dropped"),
            }
            self.db.rollback_to(keep).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Module for InscriptionsModule {
    fn name(&self) -> &'static str {
        "inscriptions"
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

    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        self.db.write_block(block, &self.config.network_type).await
    }
}
