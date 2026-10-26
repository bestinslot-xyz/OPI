use anyhow::{Context, anyhow, bail};
use opi_core::{BlockRef, BlockView};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::RunesConfig;
use crate::report::{DB_VERSION, INDEXER_VERSION};
use crate::rules::{
    block_event_hash, cumulative_hash, event_str, js_safe, outpoint_rows, spent_outpoints,
    symbol_hex, wallet_addr,
};

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
    /// Connects, creates the schema and network row on an empty database and
    /// requires `runes_indexer_version.db_version` and `runes_network_type` to
    /// match otherwise. The stored `indexer_version` is set to the reported
    /// `INDEXER_VERSION`.
    pub async fn connect(config: &RunesConfig) -> anyhow::Result<Db> {
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
            .context("connecting to the runes database")?;
        let db = Db { pool };
        db.ensure_schema(&config.network_type).await?;
        Ok(db)
    }

    async fn ensure_schema(&self, network_type: &str) -> anyhow::Result<()> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('public.runes_indexer_version') IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;
        if !exists {
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(DB_INIT).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO runes_network_type (network_type) VALUES ($1)")
                .bind(network_type)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        let version: Option<i32> =
            sqlx::query_scalar("SELECT db_version FROM runes_indexer_version LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match version {
            Some(DB_VERSION) => {}
            Some(other) => bail!(
                "runes db_version {other} is not {DB_VERSION}; the database must be recreated"
            ),
            None => bail!("runes_indexer_version is empty; the database must be recreated"),
        }
        let stored: Option<String> =
            sqlx::query_scalar("SELECT network_type FROM runes_network_type LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match stored {
            Some(stored) if stored == network_type => {}
            Some(stored) => bail!(
                "the runes database holds network {stored}, NETWORK_TYPE is {network_type}; \
                 the database must be recreated"
            ),
            None => bail!("runes_network_type is empty; the database must be recreated"),
        }
        sqlx::query("UPDATE runes_indexer_version SET indexer_version = $1")
            .bind(INDEXER_VERSION)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Inserts mainnet's `1:0` (UNCOMMON•GOODS) entry when it is missing. ord
    /// creates that rune at index init, so it has no etching record but does
    /// get mints.
    pub async fn ensure_mainnet_seed(&self) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching, \
             terms_amount, terms_cap, terms_height_l, terms_height_h, terms_offset_l, \
             terms_offset_h, mints, \"number\", premine, rune_name, spacers, symbol, \
             \"timestamp\", turbo, genesis_height, last_updated_block_height) \
             SELECT '1:0', 1, 0, 0, repeat('0', 64), 1, \
             340282366920938463463374607431768211455, 840000, 1050000, NULL, NULL, 0, 0, 0, \
             'UNCOMMONGOODS', 128, '⧉', to_timestamp(0), true, 1, 1 \
             WHERE NOT EXISTS (SELECT 1 FROM runes_id_to_entry WHERE rune_id = '1:0')",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cursor(&self) -> anyhow::Result<Option<BlockRef>> {
        let row = sqlx::query(
            "SELECT block_height, block_hash FROM runes_block_hashes \
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
            sqlx::query_scalar("SELECT block_hash FROM runes_block_hashes WHERE block_height = $1")
                .bind(height as i32)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Highest block height any runes table refers to, if any row exists.
    pub async fn max_row_height(&self) -> anyhow::Result<Option<u32>> {
        let height: Option<i32> = sqlx::query_scalar(
            "SELECT GREATEST( \
             (SELECT max(block_height) FROM runes_outpoint_to_balances), \
             (SELECT max(spent_block_height) FROM runes_outpoint_to_balances), \
             (SELECT max(block_height) FROM runes_events), \
             (SELECT max(last_updated_block_height) FROM runes_id_to_entry), \
             (SELECT max(block_height) FROM runes_id_to_entry_changes), \
             (SELECT max(block_height) FROM runes_cumulative_event_hashes))",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(height.map(|h| h as u32))
    }

    /// Drops every block above `height` in one transaction: rows created
    /// above it are deleted, outpoints spent above it are unspent, and each
    /// entry updated above it gets the totals of its latest change at or
    /// below `height` (0, 0 and its genesis height without one).
    pub async fn rollback_to(&self, height: u32) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let height = height as i32;
        for statement in [
            "DELETE FROM runes_outpoint_to_balances WHERE block_height > $1",
            "UPDATE runes_outpoint_to_balances SET spent = false, spent_block_height = NULL \
             WHERE spent_block_height > $1",
            "DELETE FROM runes_events WHERE block_height > $1",
            "DELETE FROM runes_id_to_entry WHERE genesis_height > $1",
            "DELETE FROM runes_id_to_entry_changes WHERE block_height > $1",
            "UPDATE runes_id_to_entry e \
             SET burned = coalesce(c.burned, 0), mints = coalesce(c.mints, 0), \
             last_updated_block_height = coalesce(c.block_height, e.genesis_height) \
             FROM (SELECT stale.id, latest.burned, latest.mints, latest.block_height \
                   FROM runes_id_to_entry stale \
                   LEFT JOIN LATERAL (SELECT burned, mints, block_height \
                                      FROM runes_id_to_entry_changes \
                                      WHERE rune_id = stale.rune_id AND block_height <= $1 \
                                      ORDER BY block_height DESC LIMIT 1) latest ON true \
                   WHERE stale.last_updated_block_height > $1) c \
             WHERE e.id = c.id",
            "DELETE FROM runes_cumulative_event_hashes WHERE block_height > $1",
            "DELETE FROM runes_block_hashes WHERE block_height > $1",
        ] {
            sqlx::query(statement)
                .bind(height)
                .execute(&mut *tx)
                .await?;
        }
        for reset_sequence in [
            "SELECT setval('runes_id_to_entry_id_seq', max(id)) FROM runes_id_to_entry",
            "SELECT setval('runes_id_to_entry_changes_id_seq', max(id)) \
             FROM runes_id_to_entry_changes",
            "SELECT setval('runes_outpoint_to_balances_id_seq', max(id)) \
             FROM runes_outpoint_to_balances",
            "SELECT setval('runes_cumulative_event_hashes_id_seq', max(id)) \
             FROM runes_cumulative_event_hashes",
            "SELECT setval('runes_block_hashes_id_seq', max(id)) FROM runes_block_hashes",
        ] {
            sqlx::query(reset_sequence).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Writes one block atomically: etched entries, output outpoints, spends,
    /// counters, events, the event hashes when `hashed`, and the block hash.
    /// Returns the event hashes when `hashed`.
    pub async fn write_block(
        &self,
        block: &BlockView,
        network: &str,
        hashed: bool,
    ) -> anyhow::Result<Option<BlockHashes>> {
        let height = block.height as i32;
        let mut tx = self.pool.begin().await?;
        insert_entries(&mut tx, block).await?;
        insert_outpoints(&mut tx, block, network).await?;
        spend_outpoints(&mut tx, block).await?;
        apply_counters(&mut tx, block).await?;
        insert_events(&mut tx, block, network).await?;

        let hashes = if hashed {
            let previous: Option<String> = sqlx::query_scalar(
                "SELECT cumulative_event_hash FROM runes_cumulative_event_hashes \
                 WHERE block_height = $1",
            )
            .bind(height - 1)
            .fetch_optional(&mut *tx)
            .await?;
            let events: Vec<String> = block.runes.events.iter().map(event_str).collect();
            let block_event_hash = block_event_hash(&events);
            let cumulative_event_hash = cumulative_hash(previous.as_deref(), &block_event_hash);
            sqlx::query(
                "INSERT INTO runes_cumulative_event_hashes \
                 (block_height, block_event_hash, cumulative_event_hash) VALUES ($1, $2, $3)",
            )
            .bind(height)
            .bind(&block_event_hash)
            .bind(&cumulative_event_hash)
            .execute(&mut *tx)
            .await?;
            Some(BlockHashes {
                block_event_hash,
                cumulative_event_hash,
            })
        } else {
            None
        };

        sqlx::query(
            "INSERT INTO runes_block_hashes (block_height, block_hash, block_time) \
             VALUES ($1, $2, to_timestamp($3))",
        )
        .bind(height)
        .bind(&block.hash)
        .bind(i64::from(block.time))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(hashes)
    }
}

async fn insert_entries(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
) -> anyhow::Result<()> {
    let etchings = &block.runes.etchings;
    if etchings.is_empty() {
        return Ok(());
    }
    let mut rune_id = Vec::new();
    let mut rune_block = Vec::new();
    let mut divisibility = Vec::new();
    let mut etching = Vec::new();
    let mut terms_amount = Vec::new();
    let mut terms_cap = Vec::new();
    let mut height_l = Vec::new();
    let mut height_h = Vec::new();
    let mut offset_l = Vec::new();
    let mut offset_h = Vec::new();
    let mut number = Vec::new();
    let mut premine = Vec::new();
    let mut rune_name = Vec::new();
    let mut spacers = Vec::new();
    let mut symbol = Vec::new();
    let mut timestamp = Vec::new();
    let mut turbo = Vec::new();
    for entry in etchings {
        rune_id.push(entry.rune_id.clone());
        rune_block.push(
            i32::try_from(entry.block)
                .map_err(|_| anyhow!("rune {} has block {}", entry.rune_id, entry.block))?,
        );
        divisibility.push(i32::from(entry.divisibility));
        etching.push(entry.etching.clone());
        let terms = entry.terms.as_ref();
        terms_amount.push(terms.and_then(|t| t.amount).map(|v| v.to_string()));
        terms_cap.push(terms.and_then(|t| t.cap).map(|v| v.to_string()));
        height_l.push(terms.and_then(|t| t.height.0).map(js_safe));
        height_h.push(terms.and_then(|t| t.height.1).map(js_safe));
        offset_l.push(terms.and_then(|t| t.offset.0).map(js_safe));
        offset_h.push(terms.and_then(|t| t.offset.1).map(js_safe));
        number.push(entry.number.to_string());
        premine.push(entry.premine.to_string());
        rune_name.push(entry.rune.clone());
        spacers.push(i64::from(entry.spacers));
        symbol.push(entry.symbol.map(symbol_hex));
        timestamp
            .push(i64::try_from(entry.timestamp).map_err(|_| {
                anyhow!("rune {} has timestamp {}", entry.rune_id, entry.timestamp)
            })?);
        turbo.push(entry.turbo);
    }
    sqlx::query(
        "INSERT INTO runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching, \
         terms_amount, terms_cap, terms_height_l, terms_height_h, terms_offset_l, terms_offset_h, \
         mints, \"number\", premine, rune_name, spacers, symbol, \"timestamp\", turbo, \
         genesis_height, last_updated_block_height) \
         SELECT rune_id, rune_block, 0, divisibility, etching, terms_amount::numeric, \
         terms_cap::numeric, height_l, height_h, offset_l, offset_h, 0, number::numeric, \
         premine::numeric, rune_name, spacers, symbol, to_timestamp(ts), turbo, $18, $18 \
         FROM UNNEST($1::text[], $2::int4[], $3::int4[], $4::text[], $5::text[], $6::text[], \
         $7::int8[], $8::int8[], $9::int8[], $10::int8[], $11::text[], $12::text[], $13::text[], \
         $14::int8[], $15::text[], $16::int8[], $17::bool[]) WITH ORDINALITY \
         AS u(rune_id, rune_block, divisibility, etching, terms_amount, terms_cap, height_l, \
         height_h, offset_l, offset_h, number, premine, rune_name, spacers, symbol, ts, turbo, ord) \
         ORDER BY ord",
    )
    .bind(rune_id)
    .bind(rune_block)
    .bind(divisibility)
    .bind(etching)
    .bind(terms_amount)
    .bind(terms_cap)
    .bind(height_l)
    .bind(height_h)
    .bind(offset_l)
    .bind(offset_h)
    .bind(number)
    .bind(premine)
    .bind(rune_name)
    .bind(spacers)
    .bind(symbol)
    .bind(timestamp)
    .bind(turbo)
    .bind(block.height as i32)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One row per output outpoint. `rune_ids` and `balances` travel as
/// comma-joined text, since a rune id (`block:tx`) and a decimal amount hold
/// no comma.
async fn insert_outpoints(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
    network: &str,
) -> anyhow::Result<()> {
    let rows = outpoint_rows(&block.runes.events)?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut outpoint = Vec::new();
    let mut pkscript = Vec::new();
    let mut wallet = Vec::new();
    let mut rune_ids = Vec::new();
    let mut balances = Vec::new();
    for row in rows {
        wallet.push(wallet_addr(&row.pkscript, network));
        pkscript.push(hex::encode(&row.pkscript));
        outpoint.push(row.outpoint);
        rune_ids.push(row.rune_ids.join(","));
        balances.push(
            row.balances
                .iter()
                .map(u128::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    sqlx::query(
        "INSERT INTO runes_outpoint_to_balances (outpoint, pkscript, wallet_addr, rune_ids, \
         balances, block_height) \
         SELECT outpoint, pkscript, wallet_addr, string_to_array(rune_ids, ','), \
         string_to_array(balances, ',')::numeric[], $6 \
         FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::text[]) WITH ORDINALITY \
         AS u(outpoint, pkscript, wallet_addr, rune_ids, balances, ord) ORDER BY ord",
    )
    .bind(outpoint)
    .bind(pkscript)
    .bind(wallet)
    .bind(rune_ids)
    .bind(balances)
    .bind(block.height as i32)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Marks the outpoints the block's inputs spend. Each must be stored and
/// unspent; outputs of the same block are inserted before this runs.
async fn spend_outpoints(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
) -> anyhow::Result<()> {
    let spent = spent_outpoints(&block.runes.events)?;
    if spent.is_empty() {
        return Ok(());
    }
    let updated = sqlx::query(
        "UPDATE runes_outpoint_to_balances SET spent = true, spent_block_height = $1 \
         WHERE outpoint = ANY($2) AND NOT spent",
    )
    .bind(block.height as i32)
    .bind(&spent)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if updated != spent.len() as u64 {
        bail!(
            "block {} spends {} rune outpoints but {} are stored unspent",
            block.height,
            spent.len(),
            updated
        );
    }
    Ok(())
}

/// Records each counter in `runes_id_to_entry_changes` and sets the entry's
/// totals. Every counter must reach an entry.
async fn apply_counters(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
) -> anyhow::Result<()> {
    let counters = &block.runes.counters;
    if counters.is_empty() {
        return Ok(());
    }
    let rune_id: Vec<String> = counters.iter().map(|c| c.rune_id.clone()).collect();
    let burned: Vec<String> = counters.iter().map(|c| c.burned.to_string()).collect();
    let mints: Vec<String> = counters.iter().map(|c| c.mints.to_string()).collect();
    sqlx::query(
        "INSERT INTO runes_id_to_entry_changes (rune_id, burned, mints, block_height) \
         SELECT rune_id, burned::numeric, mints::numeric, $4 \
         FROM UNNEST($1::text[], $2::text[], $3::text[]) WITH ORDINALITY \
         AS u(rune_id, burned, mints, ord) ORDER BY ord",
    )
    .bind(&rune_id)
    .bind(&burned)
    .bind(&mints)
    .bind(block.height as i32)
    .execute(&mut **tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE runes_id_to_entry e \
         SET burned = u.burned::numeric, mints = u.mints::numeric, \
         last_updated_block_height = $4 \
         FROM UNNEST($1::text[], $2::text[], $3::text[]) AS u(rune_id, burned, mints) \
         WHERE e.rune_id = u.rune_id",
    )
    .bind(&rune_id)
    .bind(&burned)
    .bind(&mints)
    .bind(block.height as i32)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if updated != counters.len() as u64 {
        bail!(
            "block {} has {} rune counters but {} entries exist for them",
            block.height,
            counters.len(),
            updated
        );
    }
    Ok(())
}

/// Inserts the events in feed order with ids continuing from the highest
/// stored id, so ids ascend in event order.
async fn insert_events(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
    network: &str,
) -> anyhow::Result<()> {
    let events = &block.runes.events;
    if events.is_empty() {
        return Ok(());
    }
    let event_type: Vec<i32> = events.iter().map(|e| i32::from(e.kind.id())).collect();
    let txid: Vec<String> = events.iter().map(|e| e.txid.clone()).collect();
    let outpoint: Vec<Option<String>> = events.iter().map(|e| e.outpoint.clone()).collect();
    let pkscript: Vec<Option<String>> = events
        .iter()
        .map(|e| e.pkscript.as_ref().map(hex::encode))
        .collect();
    let wallet: Vec<Option<String>> = events
        .iter()
        .map(|e| e.pkscript.as_ref().and_then(|pk| wallet_addr(pk, network)))
        .collect();
    let rune_id: Vec<String> = events.iter().map(|e| e.rune_id.clone()).collect();
    let amount: Vec<String> = events.iter().map(|e| e.amount.to_string()).collect();
    sqlx::query(
        "INSERT INTO runes_events (id, event_type, txid, outpoint, pkscript, wallet_addr, \
         rune_id, amount, block_height) \
         SELECT base.id + u.ord, u.event_type, u.txid, u.outpoint, u.pkscript, u.wallet_addr, \
         u.rune_id, u.amount::numeric, $8 \
         FROM (SELECT coalesce(max(id), 0) AS id FROM runes_events) base, \
         UNNEST($1::int4[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], \
         $7::text[]) WITH ORDINALITY \
         AS u(event_type, txid, outpoint, pkscript, wallet_addr, rune_id, amount, ord) \
         ORDER BY u.ord",
    )
    .bind(event_type)
    .bind(txid)
    .bind(outpoint)
    .bind(pkscript)
    .bind(wallet)
    .bind(rune_id)
    .bind(amount)
    .bind(block.height as i32)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
