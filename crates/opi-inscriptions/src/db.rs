use std::collections::{HashMap, HashSet};

use anyhow::{Context, bail};
use opi_core::{BlockRef, BlockView, metrics::PoolStats};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::rules::{address, content_type_text, event_id, media_type, split_satpoint};
use crate::{DB_VERSION, INDEXER_VERSION, InscriptionsConfig};

const DB_INIT: &str = include_str!("db_init.sql");

pub struct Db {
    pool: PgPool,
}

/// An inscription's location and owner.
#[derive(Debug, Clone)]
struct Location {
    outpoint: String,
    offset: i64,
    pkscript: String,
    address: Option<String>,
    last_moved_height: i32,
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
    id: i64,
    sequence_number: i64,
    inscription_id: String,
    old: Option<Location>,
    new: Location,
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
    /// the events and the genesis heights cover every row of the block.
    pub async fn max_row_height(&self) -> anyhow::Result<Option<u32>> {
        let height: Option<i32> = sqlx::query_scalar(
            "SELECT GREATEST( \
             (SELECT max(genesis_height) FROM inscriptions_entries), \
             (SELECT max(block_height) FROM inscriptions_events), \
             (SELECT max(block_height) FROM inscriptions_block_hashes))",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(height.map(|h| h as u32))
    }

    /// Drops every block above `height` in one transaction, or every block
    /// when `height` is `None`: each inscription transferred above it gets
    /// back the location its first transfer above it recorded, inscriptions
    /// created above it are deleted, and so are the events and block hashes
    /// above it.
    pub async fn rollback_to(&self, height: Option<u32>) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let height = height.map_or(-1, i64::from);
        for statement in [
            "UPDATE inscriptions_entries e \
             SET outpoint = t.old_outpoint, \"offset\" = t.old_offset, \
             pkscript = t.old_pkscript, address = t.old_address, \
             last_moved_height = t.old_last_moved_height \
             FROM (SELECT DISTINCT ON (sequence_number) sequence_number, old_outpoint, \
                   old_offset, old_pkscript, old_address, old_last_moved_height \
                   FROM inscriptions_events \
                   WHERE block_height > $1 AND old_outpoint IS NOT NULL \
                   ORDER BY sequence_number, id) t \
             WHERE e.sequence_number = t.sequence_number",
            "DELETE FROM inscriptions_entries WHERE genesis_height > $1",
            "DELETE FROM inscriptions_events WHERE block_height > $1",
            "DELETE FROM inscriptions_block_hashes WHERE block_height > $1",
        ] {
            sqlx::query(statement)
                .bind(height)
                .execute(&mut *tx)
                .await?;
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

    /// Deletes, in one transaction, the events and block hashes below
    /// `height`. A rollback to `height` or above reads only events above it,
    /// and the API reads the highest block hash and the events of recent
    /// blocks only.
    pub async fn prune_below(&self, height: u32) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        for statement in [
            "DELETE FROM inscriptions_events WHERE block_height < $1",
            "DELETE FROM inscriptions_block_hashes WHERE block_height < $1",
        ] {
            sqlx::query(statement)
                .bind(height as i32)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Writes one block atomically: applies its moves in order, then writes
    /// the new entries, the moved entries' final locations, one event row
    /// per move, and the block hash. A transfer of an inscription that is
    /// not stored, or a second creation of one that is, fails the block.
    pub async fn write_block(&self, block: &BlockView, network: &str) -> anyhow::Result<()> {
        let height = i32::try_from(block.height)?;
        let mut tx = self.pool.begin().await?;
        if !block.inscription_moves.is_empty() {
            apply_moves(&mut tx, block, height, network).await?;
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

async fn apply_moves(
    tx: &mut Transaction<'_, Postgres>,
    block: &BlockView,
    height: i32,
    network: &str,
) -> anyhow::Result<()> {
    let moves = &block.inscription_moves;
    let transferred: Vec<i64> = moves
        .iter()
        .filter(|m| m.genesis.is_none())
        .map(|m| i64::from(m.sequence_number))
        .collect();
    let mut locations = HashMap::<i64, Location>::new();
    if !transferred.is_empty() {
        let rows = sqlx::query(
            "SELECT sequence_number, outpoint, \"offset\", pkscript, address, last_moved_height \
             FROM inscriptions_entries WHERE sequence_number = ANY($1)",
        )
        .bind(&transferred)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            locations.insert(
                row.get(0),
                Location {
                    outpoint: row.get(1),
                    offset: row.get(2),
                    pkscript: row.get(3),
                    address: row.get(4),
                    last_moved_height: row.get(5),
                },
            );
        }
    }
    let mut created = Vec::new();
    let mut moved = Vec::new();
    let mut events = Vec::with_capacity(moves.len());
    for (position, m) in moves.iter().enumerate() {
        let sequence_number = i64::from(m.sequence_number);
        let (outpoint, offset) = split_satpoint(&m.satpoint)?;
        let new = Location {
            outpoint,
            offset: i64::try_from(offset)?,
            pkscript: hex::encode(&m.pkscript),
            address: address(&m.pkscript, network),
            last_moved_height: height,
        };
        let old = match &m.genesis {
            Some(genesis) => {
                if locations.contains_key(&sequence_number) {
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
                let Some(old) = locations.get(&sequence_number) else {
                    bail!(
                        "block {} transfers inscription {}, which is not stored",
                        block.height,
                        m.inscription_id
                    );
                };
                moved.push(sequence_number);
                Some(old.clone())
            }
        };
        locations.insert(sequence_number, new.clone());
        events.push(Event {
            id: event_id(block.height, position)?,
            sequence_number,
            inscription_id: m.inscription_id.clone(),
            old,
            new,
        });
    }
    insert_created(tx, &created, &locations, height).await?;
    let created_here: HashSet<i64> = created.iter().map(|c| c.sequence_number).collect();
    moved.retain(|seq| !created_here.contains(seq));
    moved.sort_unstable();
    moved.dedup();
    update_moved(tx, &moved, &locations).await?;
    insert_events(tx, &events, height).await?;
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
    let mut outpoint = Vec::new();
    let mut offset = Vec::new();
    let mut pkscript = Vec::new();
    let mut wallet = Vec::new();
    let mut last_moved = Vec::new();
    for c in created {
        let location = &locations[&c.sequence_number];
        sequence_number.push(c.sequence_number);
        inscription_id.push(c.inscription_id.clone());
        number.push(c.number);
        media.push(c.content_type.as_deref().and_then(media_type));
        content_type.push(c.content_type.clone());
        charms.push(c.charms);
        outpoint.push(location.outpoint.clone());
        offset.push(location.offset);
        pkscript.push(location.pkscript.clone());
        wallet.push(location.address.clone());
        last_moved.push(location.last_moved_height);
    }
    sqlx::query(
        "INSERT INTO inscriptions_entries (sequence_number, inscription_id, \"number\", \
         content_type, media_type, charms, outpoint, \"offset\", pkscript, address, \
         genesis_height, last_moved_height) \
         SELECT sequence_number, inscription_id, number, content_type, media_type, charms, \
         outpoint, \"offset\", pkscript, address, $12, last_moved_height \
         FROM UNNEST($1::int8[], $2::text[], $3::int8[], $4::text[], $5::text[], $6::int4[], \
         $7::text[], $8::int8[], $9::text[], $10::text[], $11::int4[]) \
         AS u(sequence_number, inscription_id, number, content_type, media_type, charms, \
         outpoint, \"offset\", pkscript, address, last_moved_height)",
    )
    .bind(sequence_number)
    .bind(inscription_id)
    .bind(number)
    .bind(content_type)
    .bind(media)
    .bind(charms)
    .bind(outpoint)
    .bind(offset)
    .bind(pkscript)
    .bind(wallet)
    .bind(last_moved)
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
    let mut outpoint = Vec::new();
    let mut offset = Vec::new();
    let mut pkscript = Vec::new();
    let mut wallet = Vec::new();
    let mut last_moved = Vec::new();
    for seq in moved {
        let location = &locations[seq];
        outpoint.push(location.outpoint.clone());
        offset.push(location.offset);
        pkscript.push(location.pkscript.clone());
        wallet.push(location.address.clone());
        last_moved.push(location.last_moved_height);
    }
    let updated = sqlx::query(
        "UPDATE inscriptions_entries e \
         SET outpoint = u.outpoint, \"offset\" = u.\"offset\", pkscript = u.pkscript, \
         address = u.address, last_moved_height = u.last_moved_height \
         FROM UNNEST($1::int8[], $2::text[], $3::int8[], $4::text[], $5::text[], $6::int4[]) \
         AS u(sequence_number, outpoint, \"offset\", pkscript, address, last_moved_height) \
         WHERE e.sequence_number = u.sequence_number",
    )
    .bind(moved)
    .bind(outpoint)
    .bind(offset)
    .bind(pkscript)
    .bind(wallet)
    .bind(last_moved)
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

async fn insert_events(
    tx: &mut Transaction<'_, Postgres>,
    events: &[Event],
    height: i32,
) -> anyhow::Result<()> {
    let mut id = Vec::new();
    let mut sequence_number = Vec::new();
    let mut inscription_id = Vec::new();
    let mut old_outpoint = Vec::new();
    let mut old_offset = Vec::new();
    let mut old_pkscript = Vec::new();
    let mut old_address = Vec::new();
    let mut old_last_moved = Vec::new();
    let mut new_outpoint = Vec::new();
    let mut new_offset = Vec::new();
    let mut new_pkscript = Vec::new();
    let mut new_address = Vec::new();
    for event in events {
        id.push(event.id);
        sequence_number.push(event.sequence_number);
        inscription_id.push(event.inscription_id.clone());
        let old = event.old.as_ref();
        old_outpoint.push(old.map(|o| o.outpoint.clone()));
        old_offset.push(old.map(|o| o.offset));
        old_pkscript.push(old.map(|o| o.pkscript.clone()));
        old_address.push(old.and_then(|o| o.address.clone()));
        old_last_moved.push(old.map(|o| o.last_moved_height));
        new_outpoint.push(event.new.outpoint.clone());
        new_offset.push(event.new.offset);
        new_pkscript.push(event.new.pkscript.clone());
        new_address.push(event.new.address.clone());
    }
    sqlx::query(
        "INSERT INTO inscriptions_events (id, block_height, sequence_number, inscription_id, \
         old_outpoint, old_offset, old_pkscript, old_address, old_last_moved_height, \
         new_outpoint, new_offset, new_pkscript, new_address) \
         SELECT id, $13, sequence_number, inscription_id, old_outpoint, old_offset, \
         old_pkscript, old_address, old_last_moved_height, new_outpoint, new_offset, \
         new_pkscript, new_address \
         FROM UNNEST($1::int8[], $2::int8[], $3::text[], $4::text[], $5::int8[], $6::text[], \
         $7::text[], $8::int4[], $9::text[], $10::int8[], $11::text[], $12::text[]) \
         AS u(id, sequence_number, inscription_id, old_outpoint, old_offset, old_pkscript, \
         old_address, old_last_moved_height, new_outpoint, new_offset, new_pkscript, \
         new_address)",
    )
    .bind(id)
    .bind(sequence_number)
    .bind(inscription_id)
    .bind(old_outpoint)
    .bind(old_offset)
    .bind(old_pkscript)
    .bind(old_address)
    .bind(old_last_moved)
    .bind(new_outpoint)
    .bind(new_offset)
    .bind(new_pkscript)
    .bind(new_address)
    .bind(height)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
