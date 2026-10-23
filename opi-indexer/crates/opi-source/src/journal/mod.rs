mod finalize;
pub mod order;
mod reader;
mod reorg;
mod schema;

pub use {
    finalize::{ChainData, FINALIZE_BATCH, finalize_range},
    reader::JournalReader,
    reorg::reorg_check,
    schema::*,
};

use {
    anyhow::{Context, Result, anyhow, bail},
    ord::Chain,
    redb::{Database, Durability, ReadableTable, TableDefinition, WriteTransaction},
    std::{path::Path, sync::Arc},
};

/// Table names. Each names one redb table mapping `&[u8]` to `&[u8]`.
pub mod cf {
    pub const TRANSFERS: &str = "ord_transfers";
    pub const INFO: &str = "ord_inscription_info";
    pub const HEADERS: &str = "height_to_block_header";
    pub const RAW_EVENTS: &str = "raw_events";
    pub const RAW_HEIGHTS: &str = "raw_heights";
    pub const META: &str = "meta";
}

const ALL_TABLES: [&str; 6] = [
    cf::TRANSFERS,
    cf::INFO,
    cf::HEADERS,
    cf::RAW_EVENTS,
    cf::RAW_HEIGHTS,
    cf::META,
];

const META_FINAL_TIP: &[u8] = b"final_tip";
const META_CHAIN: &[u8] = b"chain";
const META_SCHEMA: &[u8] = b"schema";
const SCHEMA_VERSION: u32 = 1;
const ZSTD_LEVEL: i32 = 3;

/// redb-backed journal of ord's raw event log and finalized records. Clones
/// share the database.
#[derive(Clone)]
pub struct Journal {
    db: Arc<Database>,
    created: bool,
}

/// Definition of the journal table called `name`.
pub fn table(name: &'static str) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
    TableDefinition::new(name)
}

impl Journal {
    /// Opens the journal at `path`, creating it for `chain` when absent.
    /// Fails when an existing journal belongs to another chain or schema.
    ///
    /// redb holds an exclusive file lock while the database is open, so a
    /// second `open` of the same path from another process fails.
    pub fn open(path: &Path, chain: Chain) -> Result<Journal> {
        let db = Database::create(path)
            .with_context(|| format!("opening journal at {}", path.display()))?;
        let chain = chain.to_string();

        let mut journal = Journal {
            db: Arc::new(db),
            created: false,
        };
        let tx = journal.begin_write()?;
        {
            for name in ALL_TABLES {
                tx.open_table(table(name))?;
            }
            let mut meta = tx.open_table(table(cf::META))?;
            let existing_chain = meta.get(META_CHAIN)?.map(|v| v.value().to_vec());
            let existing_schema = meta.get(META_SCHEMA)?.map(|v| v.value().to_vec());
            match (existing_chain, existing_schema) {
                (None, None) => {
                    meta.insert(META_CHAIN, chain.as_bytes())?;
                    meta.insert(META_SCHEMA, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                    journal.created = true;
                }
                (Some(existing), schema) => {
                    if existing != chain.as_bytes() {
                        bail!(
                            "journal chain mismatch: journal is {}, requested {chain}",
                            String::from_utf8_lossy(&existing)
                        );
                    }
                    if schema.as_deref() != Some(SCHEMA_VERSION.to_be_bytes().as_slice()) {
                        bail!("unsupported journal schema, expected {SCHEMA_VERSION}");
                    }
                }
                (None, Some(_)) => bail!("journal meta has a schema but no chain"),
            }
        }
        tx.commit()?;

        Ok(journal)
    }

    /// Begins a write transaction that commits with `Durability::Immediate`
    /// and quick repair, so a reopen after a crash skips the full-file walk.
    pub fn begin_write(&self) -> Result<WriteTransaction> {
        let mut tx = self.db.begin_write()?;
        tx.set_durability(Durability::Immediate);
        tx.set_quick_repair(true);
        Ok(tx)
    }

    /// Highest finalized height, if any block has been finalized.
    pub fn final_tip(&self) -> Result<Option<u32>> {
        let tx = self.db.begin_read()?;
        let meta = tx.open_table(table(cf::META))?;
        let Some(raw) = meta.get(META_FINAL_TIP)? else {
            return Ok(None);
        };
        let raw = raw.value();
        let bytes: [u8; 4] = raw
            .try_into()
            .map_err(|_| anyhow!("final_tip is {} bytes, expected 4", raw.len()))?;
        Ok(Some(u32::from_be_bytes(bytes)))
    }

    /// Whether this open created the journal's meta, so the journal held no
    /// data before it.
    pub fn created(&self) -> bool {
        self.created
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }
}

/// Sets `final_tip` to `height`, or clears it when `None`.
fn set_final_tip(tx: &WriteTransaction, height: Option<u32>) -> Result<()> {
    let mut meta = tx.open_table(table(cf::META))?;
    match height {
        Some(height) => {
            meta.insert(META_FINAL_TIP, height.to_be_bytes().as_slice())?;
        }
        None => {
            meta.remove(META_FINAL_TIP)?;
        }
    }
    Ok(())
}

/// Compresses an encoded inscription info value with zstd.
pub fn compress_info(value: &[u8]) -> Result<Vec<u8>> {
    Ok(zstd::bulk::compress(value, ZSTD_LEVEL)?)
}

/// Inverse of [`compress_info`].
pub fn decompress_info(stored: &[u8]) -> Result<Vec<u8>> {
    Ok(zstd::stream::decode_all(stored)?)
}
