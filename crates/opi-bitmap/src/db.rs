use anyhow::{Context, anyhow};
use opi_core::BlockRef;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Row};

use crate::BitmapConfig;
use crate::report::{DB_VERSION, INDEXER_VERSION};
use crate::rules::{Candidate, block_event_hash, cumulative_hash, event_str};

const DB_INIT: &str = include_str!("db_init.sql");

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
    /// `bitmap_indexer_version.db_version` to match otherwise. The stored
    /// `indexer_version` is set to the reported `INDEXER_VERSION`.
    pub async fn connect(config: &BitmapConfig) -> anyhow::Result<Db> {
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
            .context("connecting to the bitmap database")?;
        let db = Db { pool };
        db.ensure_schema().await?;
        Ok(db)
    }

    async fn ensure_schema(&self) -> anyhow::Result<()> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('public.bitmap_indexer_version') IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;
        if !exists {
            sqlx::raw_sql(DB_INIT).execute(&self.pool).await?;
        }
        let version: Option<i32> =
            sqlx::query_scalar("SELECT db_version FROM bitmap_indexer_version LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match version {
            Some(DB_VERSION) => {
                sqlx::query("UPDATE bitmap_indexer_version SET indexer_version = $1")
                    .bind(INDEXER_VERSION)
                    .execute(&self.pool)
                    .await?;
                Ok(())
            }
            Some(other) => Err(anyhow!(
                "bitmap db_version {other} is not {DB_VERSION}; the database must be recreated"
            )),
            None => Err(anyhow!(
                "bitmap_indexer_version is empty; the database must be recreated"
            )),
        }
    }

    pub async fn cursor(&self) -> anyhow::Result<Option<BlockRef>> {
        let row = sqlx::query(
            "SELECT block_height, block_hash FROM bitmap_block_hashes \
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
            sqlx::query_scalar(
                "SELECT block_hash FROM bitmap_block_hashes WHERE block_height = $1",
            )
            .bind(height as i32)
            .fetch_optional(&self.pool)
            .await?,
        )
    }

    /// Highest `bitmaps.block_height`, if any row exists.
    pub async fn max_bitmap_height(&self) -> anyhow::Result<Option<u32>> {
        let height: Option<i32> = sqlx::query_scalar("SELECT max(block_height) FROM bitmaps")
            .fetch_one(&self.pool)
            .await?;
        Ok(height.map(|h| h as u32))
    }

    /// Deletes every row above `height` from the three tables and resets
    /// their id sequences, in one transaction.
    pub async fn rollback_to(&self, height: u32) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let height = height as i32;
        for (delete, reset_sequence) in [
            (
                "DELETE FROM bitmaps WHERE block_height > $1",
                "SELECT setval('bitmaps_id_seq', max(id)) FROM bitmaps",
            ),
            (
                "DELETE FROM bitmap_cumulative_event_hashes WHERE block_height > $1",
                "SELECT setval('bitmap_cumulative_event_hashes_id_seq', max(id)) \
                 FROM bitmap_cumulative_event_hashes",
            ),
            (
                "DELETE FROM bitmap_block_hashes WHERE block_height > $1",
                "SELECT setval('bitmap_block_hashes_id_seq', max(id)) FROM bitmap_block_hashes",
            ),
        ] {
            sqlx::query(delete).bind(height).execute(&mut *tx).await?;
            sqlx::query(reset_sequence).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Writes the rows, event hashes and block hash of one block atomically.
    /// A number already stored, in this block or an earlier one, emits no
    /// event.
    pub async fn write_block(
        &self,
        height: u32,
        block_hash: &str,
        candidates: &[Candidate],
    ) -> anyhow::Result<BlockHashes> {
        let mut tx = self.pool.begin().await?;
        let mut events = Vec::new();
        for candidate in candidates {
            let inserted = sqlx::query(
                "INSERT INTO bitmaps (inscription_id, inscription_number, bitmap_number, \
                 block_height) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (bitmap_number) DO NOTHING",
            )
            .bind(&candidate.inscription_id)
            .bind(candidate.inscription_number)
            .bind(candidate.bitmap_number as i32)
            .bind(height as i32)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if inserted == 1 {
                events.push(event_str(
                    candidate.bitmap_number,
                    &candidate.inscription_id,
                ));
            }
        }

        let previous: Option<String> = sqlx::query_scalar(
            "SELECT cumulative_event_hash FROM bitmap_cumulative_event_hashes \
             WHERE block_height = $1",
        )
        .bind(height as i32 - 1)
        .fetch_optional(&mut *tx)
        .await?;
        let block_event_hash = block_event_hash(&events);
        let cumulative_event_hash = cumulative_hash(previous.as_deref(), &block_event_hash);
        sqlx::query(
            "INSERT INTO bitmap_cumulative_event_hashes \
             (block_height, block_event_hash, cumulative_event_hash) VALUES ($1, $2, $3)",
        )
        .bind(height as i32)
        .bind(&block_event_hash)
        .bind(&cumulative_event_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO bitmap_block_hashes (block_height, block_hash) VALUES ($1, $2)")
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
