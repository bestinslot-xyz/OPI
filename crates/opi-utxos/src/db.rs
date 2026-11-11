use anyhow::{Context, bail};
use opi_core::{BlockRef, metrics::PoolStats};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::UtxosConfig;
use crate::delta::{Delta, Output, Spend};

const DB_INIT: &str = include_str!("db_init.sql");

/// `utxos_indexer_version.db_version` this build writes.
pub const DB_VERSION: i32 = 1;

/// Whether a write keeps what a rollback needs to undo it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Undo {
    Keep,
    /// The blocks are below every rollback the module may be asked for.
    Skip,
}

pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Connects, creates the schema on an empty database and requires
    /// `utxos_indexer_version.db_version` and `utxos_network_type` to match
    /// otherwise.
    pub async fn connect(config: &UtxosConfig) -> anyhow::Result<Db> {
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
            .context("connecting to the utxos database")?;
        let db = Db { pool };
        db.ensure_schema(&config.network_type).await?;
        Ok(db)
    }

    /// The pool's open and idle connections.
    pub fn pool_stats(&self) -> PoolStats {
        PoolStats::new(self.pool.size(), self.pool.num_idle())
    }

    async fn ensure_schema(&self, network_type: &str) -> anyhow::Result<()> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('public.utxos_indexer_version') IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;
        if !exists {
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(DB_INIT).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO utxos_network_type (network_type) VALUES ($1)")
                .bind(network_type)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        let version: Option<i32> =
            sqlx::query_scalar("SELECT db_version FROM utxos_indexer_version LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match version {
            Some(DB_VERSION) => {}
            Some(other) => bail!(
                "utxos db_version {other} is not {DB_VERSION}; the database must be recreated"
            ),
            None => bail!("utxos_indexer_version is empty; the database must be recreated"),
        }
        let stored: Option<String> =
            sqlx::query_scalar("SELECT network_type FROM utxos_network_type LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match stored {
            Some(stored) if stored == network_type => Ok(()),
            Some(stored) => bail!(
                "the utxos database holds network {stored}, NETWORK_TYPE is {network_type}; \
                 the database must be recreated"
            ),
            None => bail!("utxos_network_type is empty; the database must be recreated"),
        }
    }

    pub async fn cursor(&self) -> anyhow::Result<Option<BlockRef>> {
        let row = sqlx::query(
            "SELECT block_height, block_hash FROM utxos_block_hashes \
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
            sqlx::query_scalar("SELECT block_hash FROM utxos_block_hashes WHERE block_height = $1")
                .bind(height as i32)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// The lowest height a rollback can undo.
    pub async fn undo_from(&self) -> anyhow::Result<u32> {
        let undo_from: i32 = sqlx::query_scalar("SELECT undo_from FROM utxos_undo")
            .fetch_one(&self.pool)
            .await?;
        Ok(undo_from as u32)
    }

    /// Writes the consecutive blocks `blocks` (height and hash, ascending),
    /// whose net change is `delta`, in one transaction. Fails when a spend
    /// names an output the table does not hold. With `Undo::Skip` the blocks
    /// leave nothing to undo them by, and rollbacks stop above them.
    pub async fn write_blocks(
        &self,
        blocks: &[(u32, String)],
        delta: Delta,
        undo: Undo,
    ) -> anyhow::Result<()> {
        let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
            return Ok(());
        };
        let (first, last) = (first.0, last.0);
        let (created, spent) = delta.into_parts();
        let mut tx = self.pool.begin().await?;
        insert_outputs(&mut tx, &created).await?;
        let removed = delete_spent(&mut tx, &spent, undo).await?;
        if removed != spent.len() as u64 {
            bail!(
                "blocks {first}..={last} spend {} outputs, but the table holds only {removed} of them",
                spent.len()
            );
        }
        if undo == Undo::Keep {
            let (txids, vouts, heights) =
                columns3(&created, |o| (o.txid.to_vec(), o.vout, o.height));
            sqlx::query(
                "INSERT INTO utxos_created (txid, vout, height) \
                 SELECT * FROM unnest($1::bytea[], $2::int4[], $3::int4[]) \
                 ON CONFLICT (txid, vout) DO UPDATE SET height = excluded.height",
            )
            .bind(txids)
            .bind(vouts)
            .bind(heights)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("UPDATE utxos_undo SET undo_from = greatest(undo_from, $1)")
                .bind(last as i32 + 1)
                .execute(&mut *tx)
                .await?;
        }
        let (heights, hashes): (Vec<i32>, Vec<String>) = blocks
            .iter()
            .map(|(height, hash)| (*height as i32, hash.clone()))
            .unzip();
        sqlx::query(
            "INSERT INTO utxos_block_hashes (block_height, block_hash) \
             SELECT * FROM unnest($1::int4[], $2::text[])",
        )
        .bind(heights)
        .bind(hashes)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Undoes every block above `height`: deletes the outputs they created,
    /// puts back the outputs they spent that an earlier block created, and
    /// drops their hashes. Fails below `undo_from - 1`.
    pub async fn rollback_to(&self, height: u32) -> anyhow::Result<()> {
        let undo_from = self.undo_from().await?;
        if height + 1 < undo_from {
            bail!(
                "the utxos module keeps undo data from height {undo_from}; it cannot roll back \
                 to {height}, the database must be recreated"
            );
        }
        let height = height as i32;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM utxos u USING utxos_created c \
             WHERE c.height > $1 AND u.txid = c.txid AND u.vout = c.vout",
        )
        .bind(height)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO utxos (txid, vout, value, height, coinbase, pkscript) \
             SELECT txid, vout, value, height, coinbase, pkscript FROM utxos_spent \
             WHERE spent_height > $1 AND height <= $1",
        )
        .bind(height)
        .execute(&mut *tx)
        .await?;
        for delete in [
            "DELETE FROM utxos_spent WHERE spent_height > $1",
            "DELETE FROM utxos_created WHERE height > $1",
            "DELETE FROM utxos_block_hashes WHERE block_height > $1",
        ] {
            sqlx::query(delete).bind(height).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes the undo data and block hashes below `height`; rollbacks then
    /// reach down to `height - 1`.
    pub async fn prune_below(&self, height: u32) -> anyhow::Result<()> {
        let height = height as i32;
        let mut tx = self.pool.begin().await?;
        for delete in [
            "DELETE FROM utxos_spent WHERE spent_height < $1",
            "DELETE FROM utxos_created WHERE height < $1",
            "DELETE FROM utxos_block_hashes WHERE block_height < $1",
            "UPDATE utxos_undo SET undo_from = greatest(undo_from, $1)",
        ] {
            sqlx::query(delete).bind(height).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// Inserts `outputs`. A repeated outpoint (the two BIP30 duplicate coinbases)
/// replaces the stored output, as bitcoind replaces it.
async fn insert_outputs(
    tx: &mut Transaction<'_, Postgres>,
    outputs: &[Output],
) -> anyhow::Result<()> {
    if outputs.is_empty() {
        return Ok(());
    }
    let mut txids = Vec::with_capacity(outputs.len());
    let mut vouts = Vec::with_capacity(outputs.len());
    let mut values = Vec::with_capacity(outputs.len());
    let mut heights = Vec::with_capacity(outputs.len());
    let mut coinbases = Vec::with_capacity(outputs.len());
    let mut pkscripts = Vec::with_capacity(outputs.len());
    for output in outputs {
        txids.push(output.txid.to_vec());
        vouts.push(output.vout as i32);
        values.push(output.value as i64);
        heights.push(output.height as i32);
        coinbases.push(output.coinbase);
        pkscripts.push(output.pkscript.clone());
    }
    sqlx::query(
        "INSERT INTO utxos (txid, vout, value, height, coinbase, pkscript) \
         SELECT * FROM unnest($1::bytea[], $2::int4[], $3::int8[], $4::int4[], $5::bool[], \
         $6::bytea[]) \
         ON CONFLICT (txid, vout) DO UPDATE SET value = excluded.value, \
         height = excluded.height, coinbase = excluded.coinbase, pkscript = excluded.pkscript",
    )
    .bind(txids)
    .bind(vouts)
    .bind(values)
    .bind(heights)
    .bind(coinbases)
    .bind(pkscripts)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes the spent outputs, moving them to `utxos_spent` with `Undo::Keep`.
/// Returns how many the table held.
async fn delete_spent(
    tx: &mut Transaction<'_, Postgres>,
    spent: &[Spend],
    undo: Undo,
) -> anyhow::Result<u64> {
    if spent.is_empty() {
        return Ok(0);
    }
    let (txids, vouts, heights) = columns3(spent, |s| (s.txid.to_vec(), s.vout, s.height));
    let query = match undo {
        Undo::Keep => {
            "WITH s AS (SELECT * FROM unnest($1::bytea[], $2::int4[], $3::int4[]) \
             AS s(txid, vout, spent_height)), \
             gone AS (DELETE FROM utxos u USING s WHERE u.txid = s.txid AND u.vout = s.vout \
             RETURNING u.txid, u.vout, u.value, u.height, u.coinbase, u.pkscript, s.spent_height) \
             INSERT INTO utxos_spent (txid, vout, value, height, coinbase, pkscript, spent_height) \
             SELECT * FROM gone"
        }
        Undo::Skip => {
            "DELETE FROM utxos u \
             USING unnest($1::bytea[], $2::int4[], $3::int4[]) AS s(txid, vout, spent_height) \
             WHERE u.txid = s.txid AND u.vout = s.vout"
        }
    };
    Ok(sqlx::query(query)
        .bind(txids)
        .bind(vouts)
        .bind(heights)
        .execute(&mut **tx)
        .await?
        .rows_affected())
}

/// Txid, vout and height columns of `items`, for `unnest`.
fn columns3<T>(
    items: &[T],
    row: impl Fn(&T) -> (Vec<u8>, u32, u32),
) -> (Vec<Vec<u8>>, Vec<i32>, Vec<i32>) {
    let mut txids = Vec::with_capacity(items.len());
    let mut vouts = Vec::with_capacity(items.len());
    let mut heights = Vec::with_capacity(items.len());
    for item in items {
        let (txid, vout, height) = row(item);
        txids.push(txid);
        vouts.push(vout as i32);
        heights.push(height as i32);
    }
    (txids, vouts, heights)
}
