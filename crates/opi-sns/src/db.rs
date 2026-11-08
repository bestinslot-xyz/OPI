use anyhow::{Context, anyhow};
use opi_core::{BlockRef, metrics::PoolStats};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Row};

use crate::SnsConfig;
use crate::report::DB_VERSION;
use crate::rules::{
    Candidate, SnsOp, block_event_hash, cumulative_hash, ns_register_event_str, register_event_str,
};

const DB_INIT: &str = include_str!("db_init.sql");

/// The index behind the API's name prefix search and its name order: a
/// btree in the `C` collation serves `"name" COLLATE "C" LIKE 'prefix%'` and
/// `ORDER BY "name" COLLATE "C"` under any database collation. A start
/// creates it when missing.
const NAME_PREFIX_INDEX: &str = "CREATE INDEX IF NOT EXISTS sns_names_name_c_idx \
     ON public.sns_names USING btree (\"name\" COLLATE \"C\")";

/// The finished hashes of one block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHashes {
    pub block_event_hash: String,
    pub cumulative_event_hash: String,
}

pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Connects, creates the schema on an empty database and requires
    /// `sns_names_indexer_version.db_version` to match otherwise.
    pub async fn connect(config: &SnsConfig) -> anyhow::Result<Db> {
        let mut options = PgConnectOptions::new()
            .host(&config.db_host)
            .port(config.db_port)
            .username(&config.db_user)
            .database(&config.db_database)
            .ssl_mode(if config.db_ssl {
                PgSslMode::Require
            } else {
                PgSslMode::Prefer
            });
        if let Some(password) = &config.db_password {
            options = options.password(password);
        }
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .context("connecting to the sns database")?;
        let db = Db { pool };
        db.ensure_schema().await?;
        Ok(db)
    }

    /// The pool's open and idle connections.
    pub fn pool_stats(&self) -> PoolStats {
        PoolStats::new(self.pool.size(), self.pool.num_idle())
    }

    async fn ensure_schema(&self) -> anyhow::Result<()> {
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regclass('public.sns_names_indexer_version') IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            sqlx::raw_sql(DB_INIT).execute(&self.pool).await?;
        }
        let version: Option<i32> =
            sqlx::query_scalar("SELECT db_version FROM sns_names_indexer_version LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match version {
            Some(DB_VERSION) => {
                // The lookup comes first, so a start that finds the index
                // never waits on the table lock.
                let present: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_indexes \
                     WHERE schemaname = 'public' AND indexname = 'sns_names_name_c_idx')",
                )
                .fetch_one(&self.pool)
                .await?;
                if !present {
                    sqlx::query(NAME_PREFIX_INDEX).execute(&self.pool).await?;
                }
                Ok(())
            }
            Some(other) => Err(anyhow!(
                "sns db_version {other} is not {DB_VERSION}; the database must be recreated"
            )),
            None => Err(anyhow!(
                "sns_names_indexer_version is empty; the database must be recreated"
            )),
        }
    }

    pub async fn cursor(&self) -> anyhow::Result<Option<BlockRef>> {
        let row = sqlx::query(
            "SELECT block_height, block_hash FROM sns_block_hashes \
             ORDER BY block_height DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| BlockRef {
            height: row.get::<i32, _>(0) as u32,
            hash: row.get(1),
        }))
    }

    pub async fn block_hash(&self, height: u32) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT block_hash FROM sns_block_hashes WHERE block_height = $1")
                .bind(height as i32)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Highest `block_height` over `sns_names` and `sns_namespaces`, if any
    /// row exists.
    pub async fn max_row_height(&self) -> anyhow::Result<Option<u32>> {
        let height: Option<i32> = sqlx::query_scalar(
            "SELECT greatest((SELECT max(block_height) FROM sns_names), \
             (SELECT max(block_height) FROM sns_namespaces))",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(height.map(|h| h as u32))
    }

    /// Deletes every row above `height` from the four tables and resets
    /// their id sequences, in one transaction.
    pub async fn rollback_to(&self, height: u32) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let height = height as i32;
        for (delete, reset_sequence) in [
            (
                "DELETE FROM sns_names WHERE block_height > $1",
                "SELECT setval('sns_names_id_seq', max(id)) FROM sns_names",
            ),
            (
                "DELETE FROM sns_namespaces WHERE block_height > $1",
                "SELECT setval('sns_namespaces_id_seq', max(id)) FROM sns_namespaces",
            ),
            (
                "DELETE FROM sns_names_cumulative_event_hashes WHERE block_height > $1",
                "SELECT setval('sns_names_cumulative_event_hashes_id_seq', max(id)) \
                 FROM sns_names_cumulative_event_hashes",
            ),
            (
                "DELETE FROM sns_block_hashes WHERE block_height > $1",
                "SELECT setval('sns_block_hashes_id_seq', max(id)) FROM sns_block_hashes",
            ),
        ] {
            sqlx::query(delete).bind(height).execute(&mut *tx).await?;
            sqlx::query(reset_sequence).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes the block hashes below `height`. The API reads only the
    /// highest, and a rollback walks no deeper than the pruning window.
    pub async fn prune_below(&self, height: u32) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM sns_block_hashes WHERE block_height < $1")
            .bind(height as i32)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Writes the rows, event hashes and block hash of one block atomically.
    /// A name or namespace already stored, in this block or an earlier one,
    /// emits no event.
    pub async fn write_block(
        &self,
        height: u32,
        block_hash: &str,
        candidates: &[Candidate],
    ) -> anyhow::Result<BlockHashes> {
        let mut tx = self.pool.begin().await?;
        let mut events = Vec::new();
        for candidate in candidates {
            let id = &candidate.inscription_id;
            let (inserted, event) = match &candidate.op {
                SnsOp::Name { name, domain } => (
                    sqlx::query(
                        "INSERT INTO sns_names (inscription_id, inscription_number, \"name\", \
                         domain, block_height) VALUES ($1, $2, $3, $4, $5) \
                         ON CONFLICT (\"name\") DO NOTHING",
                    )
                    .bind(id)
                    .bind(candidate.inscription_number)
                    .bind(name)
                    .bind(domain)
                    .bind(height as i32)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected(),
                    register_event_str(name, domain, id),
                ),
                SnsOp::Namespace(namespace) => (
                    sqlx::query(
                        "INSERT INTO sns_namespaces (inscription_id, inscription_number, \
                         \"namespace\", block_height) VALUES ($1, $2, $3, $4) \
                         ON CONFLICT (\"namespace\") DO NOTHING",
                    )
                    .bind(id)
                    .bind(candidate.inscription_number)
                    .bind(namespace)
                    .bind(height as i32)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected(),
                    ns_register_event_str(namespace, id),
                ),
            };
            if inserted == 1 {
                events.push(event);
            }
        }

        let previous: Option<String> = sqlx::query_scalar(
            "SELECT cumulative_event_hash FROM sns_names_cumulative_event_hashes \
             WHERE block_height = $1",
        )
        .bind(height as i32 - 1)
        .fetch_optional(&mut *tx)
        .await?;
        let block_event_hash = block_event_hash(&events);
        let cumulative_event_hash = cumulative_hash(previous.as_deref(), &block_event_hash);
        sqlx::query(
            "INSERT INTO sns_names_cumulative_event_hashes \
             (block_height, block_event_hash, cumulative_event_hash) VALUES ($1, $2, $3)",
        )
        .bind(height as i32)
        .bind(&block_event_hash)
        .bind(&cumulative_event_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO sns_block_hashes (block_height, block_hash) VALUES ($1, $2)")
            .bind(height as i32)
            .bind(block_hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(BlockHashes {
            block_event_hash,
            cumulative_event_hash,
        })
    }
}
