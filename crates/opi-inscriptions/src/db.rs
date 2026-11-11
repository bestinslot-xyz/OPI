use std::collections::{HashMap, HashSet};

use anyhow::{Context, bail};
use opi_core::{BlockRef, BlockView, metrics::PoolStats};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::rules::{
    content_type_text, event_id, first_event_id, media_type, split_outpoint, split_satpoint,
};
use crate::{DB_VERSION, INDEXER_VERSION, InscriptionsConfig};

const DB_INIT: &str = include_str!("db_init.sql");

pub struct Db {
    pool: PgPool,
}

/// An inscription's location after one of its events.
#[derive(Debug, Clone)]
struct Location {
    txid: Vec<u8>,
    vout: i32,
    offset: i64,
    pkscript: Vec<u8>,
    last_moved_height: i32,
    event_id: i64,
}

/// An inscription created in the block, at its location after the block.
struct Created {
    sequence_number: i64,
    inscription_id: String,
    number: i64,
    content_type: Option<String>,
    charms: i32,
}

/// One `inscriptions_events` row.
struct Event {
    sequence_number: i64,
    prev_id: Option<i64>,
    location: Location,
}

impl Db {
    /// Connects, creates the schema and network row on an empty database and
    /// requires `inscriptions_indexer_version.db_version` and
    /// `inscriptions_network_type` to match otherwise.
    pub async fn connect(config: &InscriptionsConfig) -> anyhow::Result<Db> {
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
            .context("connecting to the inscriptions database")?;
        let db = Db { pool };
        db.ensure_schema(&config.network_type).await?;
        Ok(db)
    }

    /// The pool's open and idle connections.
    pub fn pool_stats(&self) -> PoolStats {
        PoolStats::new(self.pool.size(), self.pool.num_idle())
    }

    async fn ensure_schema(&self, network_type: &str) -> anyhow::Result<()> {
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regclass('public.inscriptions_indexer_version') IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(DB_INIT).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO inscriptions_network_type (network_type) VALUES ($1)")
                .bind(network_type)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        let version: Option<(String, i32)> = sqlx::query_as(
            "SELECT indexer_version, db_version FROM inscriptions_indexer_version LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        match version {
            Some((indexer, DB_VERSION)) if indexer == INDEXER_VERSION => {}
            Some((indexer, db)) => bail!(
                "the inscriptions database holds {indexer} db_version {db}, expected \
                 {INDEXER_VERSION} db_version {DB_VERSION}; the database must be recreated"
            ),
            None => {
                bail!("inscriptions_indexer_version is empty; the database must be recreated")
            }
        }
        let stored: Option<String> =
            sqlx::query_scalar("SELECT network_type FROM inscriptions_network_type LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        match stored {
            Some(stored) if stored == network_type => Ok(()),
            Some(stored) => bail!(
                "the inscriptions database holds network {stored}, NETWORK_TYPE is \
                 {network_type}; the database must be recreated"
            ),
            None => bail!("inscriptions_network_type is empty; the database must be recreated"),
        }
    }

    pub async fn cursor(&self) -> anyhow::Result<Option<BlockRef>> {
        let row = sqlx::query(
            "SELECT block_height, block_hash FROM inscriptions_block_hashes \
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
        Ok(sqlx::query_scalar(
            "SELECT block_hash FROM inscriptions_block_hashes WHERE block_height = $1",
        )
        .bind(height as i32)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Highest block height any row refers to. Every creation and transfer
    /// writes an event row in the same transaction as the entry change, so
    /// the events cover every entry row of the block.
    pub async fn max_row_height(&self) -> anyhow::Result<Option<u32>> {
        let height: Option<i32> = sqlx::query_scalar(
            "SELECT GREATEST( \
             (SELECT (max(id) >> 32)::int4 FROM inscriptions_events), \
             (SELECT max(block_height) FROM inscriptions_block_hashes))",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(height.map(|h| h as u32))
    }

    /// Drops every block above `height` in one transaction, or every block
    /// when `height` is `None`: each inscription transferred above it gets
    /// back the location of the event its first transfer above it names as
    /// `prev_id`, the inscriptions whose creation event is above it are
    /// deleted, and so are the events and block hashes above it.
    pub async fn rollback_to(&self, height: Option<u32>) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let height = height.map_or(-1, i64::from);
        let above = first_event_id(height + 1);
        restore_locations(&mut tx, above).await?;
        for (statement, bound) in [
            (
                "DELETE FROM inscriptions_entries WHERE sequence_number IN ( \
                 SELECT sequence_number FROM inscriptions_events \
                 WHERE id >= $1 AND prev_id IS NULL)",
                above,
            ),
            ("DELETE FROM inscriptions_events WHERE id >= $1", above),
            (
                "DELETE FROM inscriptions_block_hashes WHERE block_height > $1",
                height,
            ),
        ] {
            sqlx::query(statement).bind(bound).execute(&mut *tx).await?;
        }
        sqlx::query(
            "SELECT setval('inscriptions_block_hashes_id_seq', max(id)) \
             FROM inscriptions_block_hashes",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes, in one transaction, the block hashes below `height` and the
    /// events below it that a later event below it replaced: the `prev_id`
    /// of every event between the previous prune's height and `height`.
    /// Each inscription keeps its latest event below `height`, which a
    /// rollback to `height` or above reads through the next transfer's
    /// `prev_id`; the API reads the highest block hash and the events of
    /// recent blocks only.
    pub async fn prune_below(&self, height: u32) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let floor: Option<i32> =
            sqlx::query_scalar("SELECT min(block_height) FROM inscriptions_block_hashes")
                .fetch_one(&mut *tx)
                .await?;
        let height = i64::from(height);
        if let Some(floor) = floor.map(i64::from).filter(|floor| *floor < height) {
            sqlx::query(
                "DELETE FROM inscriptions_events WHERE id IN ( \
                 SELECT prev_id FROM inscriptions_events \
                 WHERE id >= $1 AND id < $2 AND prev_id IS NOT NULL)",
            )
            .bind(first_event_id(floor))
            .bind(first_event_id(height))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM inscriptions_block_hashes WHERE block_height < $1")
            .bind(height)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Writes one block atomically: applies its moves in order, then writes
    /// the new entries, the moved entries' final locations, one event row
    /// per move, and the block hash. A transfer of an inscription that is
    /// not stored, or a second creation of one that is, fails the block.
    pub async fn write_block(&self, block: &BlockView) -> anyhow::Result<()> {
        let height = i32::try_from(block.height)?;
        let mut tx = self.pool.begin().await?;
        if !block.inscription_moves.is_empty() {
            apply_moves(&mut tx, block, height).await?;
        }
        sqlx::query(
            "INSERT INTO inscriptions_block_hashes (block_height, block_hash, block_time) \
             VALUES ($1, $2, to_timestamp($3))",
        )
        .bind(height)
        .bind(&block.hash)
        .bind(i64::from(block.time))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// The first event at or above event id `above` of each inscription
/// transferred there, and the event its `prev_id` names, joined.
const FIRST_TRANSFERS_ABOVE: &str = "FROM (SELECT DISTINCT ON (sequence_number) \
     sequence_number, prev_id FROM inscriptions_events WHERE id >= $1 \
     ORDER BY sequence_number, id) t \
     LEFT JOIN inscriptions_events p ON p.id = t.prev_id \
     WHERE t.prev_id IS NOT NULL";

/// Puts each inscription transferred at or above event id `above` back at
/// the location of the event its first transfer there names as `prev_id`.
/// A missing `prev_id` row fails the rollback.
async fn restore_locations(tx: &mut Transaction<'_, Postgres>, above: i64) -> anyhow::Result<()> {
    let missing: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT t.prev_id {FIRST_TRANSFERS_ABOVE} AND p.id IS NULL LIMIT 1"
    )))
    .bind(above)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(prev_id) = missing {
        bail!("rollback reads event {prev_id}, which is not stored");
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE inscriptions_entries e \
         SET txid = r.txid, vout = r.vout, \"offset\" = r.\"offset\", pkscript = r.pkscript, \
         last_moved_height = (r.id >> 32)::int4, last_event_id = r.id \
         FROM (SELECT t.sequence_number, p.id, p.txid, p.vout, p.\"offset\", p.pkscript \
               {FIRST_TRANSFERS_ABOVE}) r \
         WHERE e.sequence_number = r.sequence_number"
    )))
    .bind(above)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn apply_moves(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
    height: i32,
) -> anyhow::Result<()> {
    let moves = &block.inscription_moves;
    let transferred: Vec<i64> = moves
        .iter()
        .filter(|m| m.genesis.is_none())
        .map(|m| i64::from(m.sequence_number))
        .collect();
    // each inscription's latest event: stored, then this block's
    let mut last_event = HashMap::<i64, i64>::new();
    if !transferred.is_empty() {
        let rows = sqlx::query(
            "SELECT sequence_number, last_event_id FROM inscriptions_entries \
             WHERE sequence_number = ANY($1)",
        )
        .bind(&transferred)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            last_event.insert(row.get(0), row.get(1));
        }
    }
    let mut locations = HashMap::<i64, Location>::new();
    let mut created = Vec::new();
    let mut moved = Vec::new();
    let mut events = Vec::with_capacity(moves.len());
    for (position, m) in moves.iter().enumerate() {
        let sequence_number = i64::from(m.sequence_number);
        let id = event_id(block.height, position)?;
        let (outpoint, offset) = split_satpoint(&m.satpoint)?;
        let (txid, vout) = split_outpoint(&outpoint)?;
        let location = Location {
            txid,
            vout,
            offset: i64::try_from(offset)?,
            pkscript: m.pkscript.to_vec(),
            last_moved_height: height,
            event_id: id,
        };
        let prev_id = match &m.genesis {
            Some(genesis) => {
                if last_event.contains_key(&sequence_number) {
                    bail!(
                        "block {} creates inscription {} twice",
                        block.height,
                        m.inscription_id
                    );
                }
                let content_type = genesis.content_type.as_deref().map(content_type_text);
                created.push(Created {
                    sequence_number,
                    inscription_id: m.inscription_id.clone(),
                    number: i64::from(genesis.number),
                    content_type,
                    charms: i32::from(genesis.charms),
                });
                None
            }
            None => {
                let Some(prev_id) = last_event.get(&sequence_number) else {
                    bail!(
                        "block {} transfers inscription {}, which is not stored",
                        block.height,
                        m.inscription_id
                    );
                };
                moved.push(sequence_number);
                Some(*prev_id)
            }
        };
        last_event.insert(sequence_number, id);
        locations.insert(sequence_number, location.clone());
        events.push(Event {
            sequence_number,
            prev_id,
            location,
        });
    }
    insert_created(tx, &created, &locations, height).await?;
    let created_here: HashSet<i64> = created.iter().map(|c| c.sequence_number).collect();
    moved.retain(|seq| !created_here.contains(seq));
    moved.sort_unstable();
    moved.dedup();
    update_moved(tx, &moved, &locations).await?;
    insert_events(tx, &events).await?;
    Ok(())
}

/// Inserts the inscriptions created in the block at their locations after
/// it.
async fn insert_created(
    tx: &mut Transaction<'_, Postgres>,
    created: &[Created],
    locations: &HashMap<i64, Location>,
    height: i32,
) -> anyhow::Result<()> {
    if created.is_empty() {
        return Ok(());
    }
    let mut sequence_number = Vec::new();
    let mut inscription_id = Vec::new();
    let mut number = Vec::new();
    let mut content_type = Vec::new();
    let mut media = Vec::new();
    let mut charms = Vec::new();
    let mut txid = Vec::new();
    let mut vout = Vec::new();
    let mut offset = Vec::new();
    let mut pkscript = Vec::new();
    let mut last_moved = Vec::new();
    let mut last_event = Vec::new();
    for c in created {
        let location = &locations[&c.sequence_number];
        sequence_number.push(c.sequence_number);
        inscription_id.push(c.inscription_id.clone());
        number.push(c.number);
        media.push(c.content_type.as_deref().and_then(media_type));
        content_type.push(c.content_type.clone());
        charms.push(c.charms);
        txid.push(location.txid.clone());
        vout.push(location.vout);
        offset.push(location.offset);
        pkscript.push(location.pkscript.clone());
        last_moved.push(location.last_moved_height);
        last_event.push(location.event_id);
    }
    sqlx::query(
        "INSERT INTO inscriptions_entries (sequence_number, inscription_id, \"number\", \
         content_type, media_type, charms, txid, vout, \"offset\", pkscript, \
         genesis_height, last_moved_height, last_event_id) \
         SELECT sequence_number, inscription_id, number, content_type, media_type, charms, \
         txid, vout, \"offset\", pkscript, $13, last_moved_height, last_event_id \
         FROM UNNEST($1::int8[], $2::text[], $3::int8[], $4::text[], $5::text[], $6::int4[], \
         $7::bytea[], $8::int4[], $9::int8[], $10::bytea[], $11::int4[], $12::int8[]) \
         AS u(sequence_number, inscription_id, number, content_type, media_type, charms, \
         txid, vout, \"offset\", pkscript, last_moved_height, last_event_id)",
    )
    .bind(sequence_number)
    .bind(inscription_id)
    .bind(number)
    .bind(content_type)
    .bind(media)
    .bind(charms)
    .bind(txid)
    .bind(vout)
    .bind(offset)
    .bind(pkscript)
    .bind(last_moved)
    .bind(last_event)
    .bind(height)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Sets each stored inscription moved in the block to its location after
/// the block, one update per inscription.
async fn update_moved(
    tx: &mut Transaction<'_, Postgres>,
    moved: &[i64],
    locations: &HashMap<i64, Location>,
) -> anyhow::Result<()> {
    if moved.is_empty() {
        return Ok(());
    }
    let mut txid = Vec::new();
    let mut vout = Vec::new();
    let mut offset = Vec::new();
    let mut pkscript = Vec::new();
    let mut last_moved = Vec::new();
    let mut last_event = Vec::new();
    for seq in moved {
        let location = &locations[seq];
        txid.push(location.txid.clone());
        vout.push(location.vout);
        offset.push(location.offset);
        pkscript.push(location.pkscript.clone());
        last_moved.push(location.last_moved_height);
        last_event.push(location.event_id);
    }
    let updated = sqlx::query(
        "UPDATE inscriptions_entries e \
         SET txid = u.txid, vout = u.vout, \"offset\" = u.\"offset\", pkscript = u.pkscript, \
         last_moved_height = u.last_moved_height, last_event_id = u.last_event_id \
         FROM UNNEST($1::int8[], $2::bytea[], $3::int4[], $4::int8[], $5::bytea[], \
         $6::int4[], $7::int8[]) \
         AS u(sequence_number, txid, vout, \"offset\", pkscript, last_moved_height, \
         last_event_id) \
         WHERE e.sequence_number = u.sequence_number",
    )
    .bind(moved)
    .bind(txid)
    .bind(vout)
    .bind(offset)
    .bind(pkscript)
    .bind(last_moved)
    .bind(last_event)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if updated != moved.len() as u64 {
        bail!(
            "{} inscriptions moved but {updated} entries were updated",
            moved.len()
        );
    }
    Ok(())
}

async fn insert_events(tx: &mut Transaction<'_, Postgres>, events: &[Event]) -> anyhow::Result<()> {
    let mut id = Vec::with_capacity(events.len());
    let mut sequence_number = Vec::with_capacity(events.len());
    let mut prev_id = Vec::with_capacity(events.len());
    let mut offset = Vec::with_capacity(events.len());
    let mut vout = Vec::with_capacity(events.len());
    let mut txid = Vec::with_capacity(events.len());
    let mut pkscript = Vec::with_capacity(events.len());
    for event in events {
        id.push(event.location.event_id);
        sequence_number.push(event.sequence_number);
        prev_id.push(event.prev_id);
        offset.push(event.location.offset);
        vout.push(event.location.vout);
        txid.push(event.location.txid.clone());
        pkscript.push(event.location.pkscript.clone());
    }
    sqlx::query(
        "INSERT INTO inscriptions_events (id, sequence_number, prev_id, \"offset\", vout, \
         txid, pkscript) \
         SELECT * FROM UNNEST($1::int8[], $2::int8[], $3::int8[], $4::int8[], $5::int4[], \
         $6::bytea[], $7::bytea[])",
    )
    .bind(id)
    .bind(sequence_number)
    .bind(prev_id)
    .bind(offset)
    .bind(vout)
    .bind(txid)
    .bind(pkscript)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
