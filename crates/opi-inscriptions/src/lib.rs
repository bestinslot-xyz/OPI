//! Inscription metadata as an opi module: the current location, owner and
//! content type of every bound inscription, written from the journal's
//! inscription moves into the schema in `db_init.sql`, which the
//! inscriptions API (`opi-api`) reads.
//!
//! More than [`CATCH_UP_GAP`] blocks behind the chain, the module catches up
//! without the indexes only the API reads, and builds them before it writes
//! a block within `MAX_ROLLBACK_DEPTH` of the chain's tip. Until then the
//! API's location, wallet and history lookups scan their tables.

mod db;
mod rules;

use std::sync::Arc;

use anyhow::{Context, anyhow};
use opi_core::{BlockRef, BlockView, MAX_ROLLBACK_DEPTH, Module};

pub use rules::{address, content_type_text, media_type};

use db::Db;

pub const INDEXER_VERSION: &str = "opi-inscriptions v2.0.0";
pub const DB_VERSION: i32 = 2;

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

/// Blocks a module must be behind the chain's tip to catch up without the
/// secondary indexes: building them again takes hours on a large database.
pub const CATCH_UP_GAP: u32 = 10_000;

/// The height of the chain the module catches up to.
#[async_trait::async_trait]
pub trait ChainHeight: Send + Sync {
    async fn height(&self) -> anyhow::Result<u32>;
}

/// Catching up without the secondary indexes.
struct CatchUp {
    chain: Arc<dyn ChainHeight>,
    /// The chain's height when last read; the chain only grows past it.
    tip: u32,
}

pub struct InscriptionsModule {
    db: Db,
    start_height: u32,
    catch_up: Option<CatchUp>,
}

impl InscriptionsModule {
    /// Connects, prepares the schema and drops rows left above the cursor.
    /// With a `chain` more than [`CATCH_UP_GAP`] blocks ahead of the next
    /// height to index, drops the secondary indexes; otherwise builds the
    /// ones the database lacks.
    pub async fn new(
        config: InscriptionsConfig,
        chain: Option<Arc<dyn ChainHeight>>,
    ) -> anyhow::Result<InscriptionsModule> {
        if !NETWORKS.contains(&config.network_type.as_str()) {
            return Err(anyhow!("unknown NETWORK_TYPE {}", config.network_type));
        }
        let db = Db::connect(&config).await?;
        let mut module = InscriptionsModule {
            start_height: start_height(&config.network_type),
            db,
            catch_up: None,
        };
        let next = module.drop_residue().await?;
        if let Some(chain) = chain {
            let tip = chain.height().await.context("reading the chain's height")?;
            if next.saturating_add(CATCH_UP_GAP) < tip {
                tracing::info!(
                    module = "inscriptions",
                    next,
                    tip,
                    "catching up without the secondary indexes"
                );
                module.db.drop_secondary_indexes().await?;
                module.catch_up = Some(CatchUp { chain, tip });
            }
        }
        if module.catch_up.is_none() {
            module.db.build_secondary_indexes().await?;
        }
        Ok(module)
    }

    /// Ends a catch-up when `height` is within `MAX_ROLLBACK_DEPTH` of the
    /// chain's tip, building the secondary indexes.
    async fn finish_catch_up(&mut self, height: u32) -> anyhow::Result<()> {
        let Some(catch_up) = &mut self.catch_up else {
            return Ok(());
        };
        let near = |tip: u32| height.saturating_add(MAX_ROLLBACK_DEPTH) >= tip;
        if !near(catch_up.tip) {
            return Ok(());
        }
        catch_up.tip = catch_up
            .chain
            .height()
            .await
            .context("reading the chain's height")?;
        if !near(catch_up.tip) {
            return Ok(());
        }
        tracing::info!(
            module = "inscriptions",
            height,
            tip = catch_up.tip,
            "caught up; building the secondary indexes"
        );
        self.db.build_secondary_indexes().await?;
        self.catch_up = None;
        Ok(())
    }

    /// Drops rows at or above the next height to index: above the cursor, or
    /// every row when there is no cursor. Returns that height.
    async fn drop_residue(&self) -> anyhow::Result<u32> {
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
        Ok(next)
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
        self.finish_catch_up(block.height).await?;
        self.db.write_block(block).await
    }
}
