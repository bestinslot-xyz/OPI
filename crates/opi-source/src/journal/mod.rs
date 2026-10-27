mod finalize;
pub mod order;
mod reader;
mod reorg;
mod runes;
mod schema;

pub use {
    finalize::{ChainData, EmissionOrderError, FINALIZE_BATCH, finalize_range, is_json_body},
    reader::JournalReader,
    reorg::reorg_check,
    runes::RuneModelError,
    schema::*,
};

use {
    crate::ord_settings::{enabled_or_disabled, with_or_without},
    anyhow::{Context, Result, anyhow, bail},
    ord::Chain,
    redb::{
        Database, Durability, ReadableDatabase, ReadableTable, TableDefinition, TableHandle,
        WriteTransaction,
    },
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
    /// Inscription id (store form) -> height (`u32` BE) of its recorded
    /// transfer. The fork records only an inscription's first transfer.
    pub const FIRST_TRANSFER: &str = "first_transfer";
    /// Rune events: height (`u32` BE) ++ sequence (`u32` BE) -> encoded event.
    pub const RUNE_EVENTS: &str = "rune_events";
    /// Rune etchings: height (`u32` BE) ++ sequence (`u32` BE) -> encoded
    /// etching.
    pub const RUNE_ETCHINGS: &str = "rune_etchings";
    /// Rune id (block `u64` BE ++ tx `u32` BE) -> running burned and mint
    /// totals (`u128` BE each).
    pub const RUNE_COUNTERS: &str = "rune_counters";
    /// Height (`u32` BE) ++ rune id -> totals before the block (optional)
    /// and after it. The block's counter rows and the reorg undo data.
    pub const RUNE_COUNTER_LOG: &str = "rune_counter_log";
    /// Outpoint (store form) -> pkscript and the runes it holds.
    pub const RUNE_BALANCES: &str = "rune_outpoint_balances";
}

const ALL_TABLES: [&str; 12] = [
    cf::TRANSFERS,
    cf::INFO,
    cf::HEADERS,
    cf::RAW_EVENTS,
    cf::RAW_HEIGHTS,
    cf::META,
    cf::FIRST_TRANSFER,
    cf::RUNE_EVENTS,
    cf::RUNE_ETCHINGS,
    cf::RUNE_COUNTERS,
    cf::RUNE_COUNTER_LOG,
    cf::RUNE_BALANCES,
];

const RUNE_TABLES: [&str; 5] = [
    cf::RUNE_EVENTS,
    cf::RUNE_ETCHINGS,
    cf::RUNE_COUNTERS,
    cf::RUNE_COUNTER_LOG,
    cf::RUNE_BALANCES,
];

const META_FINAL_TIP: &[u8] = b"final_tip";
const META_GATE_LIMIT: &[u8] = b"gate_limit";
const META_CHAIN: &[u8] = b"chain";
const META_SCHEMA: &[u8] = b"schema";
/// Whether the journal keeps rune records: one byte, 1 or 0, written when the
/// journal is created. A journal without the row and with finalized blocks
/// keeps them when it has a rune table; one without finalized blocks takes
/// the setting it is opened with.
const META_RUNES: &[u8] = b"runes";
const SCHEMA_VERSION: u32 = 1;
const ZSTD_LEVEL: i32 = 3;

/// redb-backed journal of ord's raw event log and finalized records. Clones
/// share the database.
#[derive(Clone)]
pub struct Journal {
    db: Arc<Database>,
    created: bool,
    runes: bool,
}

/// The tables of a journal that keeps rune records when `runes` is set.
fn tables(runes: bool) -> impl Iterator<Item = &'static str> {
    ALL_TABLES
        .into_iter()
        .filter(move |name| runes || !RUNE_TABLES.contains(name))
}

/// The scratch path a file is built at before being renamed to `path`.
pub fn init_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".init");
    name.into()
}

/// Deletes `path` when it exists.
pub(crate) fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

/// Fsyncs the directory holding `path`, making a rename into it durable.
pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", parent.display()))
}

/// Definition of the journal table called `name`.
pub fn table(name: &'static str) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
    TableDefinition::new(name)
}

impl Journal {
    /// Opens the journal at `path`, creating it for `chain` when absent, with
    /// rune records when `runes` is set. Fails when an existing journal
    /// belongs to another chain or schema, was built with the other `runes`
    /// setting, or keeps rune records and has finalized blocks without the
    /// rune tables. A refused open leaves the journal unchanged.
    ///
    /// redb holds an exclusive file lock while the database is open, so a
    /// second `open` of the same path from another process fails.
    ///
    /// A new journal is built at `<path>.init` and renamed into place once
    /// complete, so a crash during creation leaves no half-made journal.
    pub fn open(path: &Path, chain: Chain, runes: bool) -> Result<Journal> {
        let init = init_path(path);
        let created = if path.exists() {
            remove_if_exists(&init)?;
            false
        } else {
            remove_if_exists(&init)?;
            Self::initialize(&init, chain, runes)?;
            std::fs::rename(&init, path)
                .with_context(|| format!("renaming {} into place", init.display()))?;
            sync_parent(path)?;
            true
        };
        let db = Database::create(path)
            .with_context(|| format!("opening journal at {}", path.display()))?;
        let chain = chain.to_string();

        let mut journal = Journal {
            db: Arc::new(db),
            created,
            runes,
        };
        journal.check_runes(path)?;
        let tx = journal.begin_write()?;
        {
            for name in tables(runes) {
                tx.open_table(table(name))?;
            }
            let mut meta = tx.open_table(table(cf::META))?;
            if meta.get(META_RUNES)?.is_none() {
                meta.insert(META_RUNES, [u8::from(runes)].as_slice())?;
            }
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

    /// Fails when the journal was built with the other setting than
    /// `self.runes`, or keeps rune records and has finalized blocks but lacks
    /// a rune table: its finalized heights then carry no rune state, so the
    /// journal and ord's data dir are rebuilt together. A journal without the
    /// runes meta row and without finalized blocks takes `self.runes`.
    fn check_runes(&self, path: &Path) -> Result<()> {
        let tx = self.db.begin_read()?;
        let names: Vec<String> = tx
            .list_tables()?
            .map(|handle| handle.name().to_string())
            .collect();
        let has = |name: &str| names.iter().any(|have| have == name);
        if !has(cf::META) {
            return Ok(());
        }
        let tip = final_tip_in(&tx)?;
        let built = match tx.open_table(table(cf::META))?.get(META_RUNES)? {
            Some(raw) => match raw.value() {
                [0] => false,
                [1] => true,
                other => bail!("journal runes setting is {other:?}, expected [0] or [1]"),
            },
            None if tip.is_none() => self.runes,
            None => RUNE_TABLES.iter().any(|name| has(name)),
        };
        if built != self.runes {
            bail!(
                "journal at {} was built {} rune indexing, but runes is {}; start with runes {} \
                 again, or delete the journal and ord's data dir and index again",
                path.display(),
                with_or_without(built),
                enabled_or_disabled(self.runes),
                enabled_or_disabled(built),
            );
        }
        if self.runes && tip.is_some() && !RUNE_TABLES.iter().all(|name| has(name)) {
            bail!(
                "journal at {} predates rune indexing; delete it and ord's data dir and \
                 index again",
                path.display()
            );
        }
        Ok(())
    }

    /// Creates the journal file at `path` with its tables and the meta rows
    /// in one durable transaction.
    fn initialize(path: &Path, chain: Chain, runes: bool) -> Result<()> {
        let db = Database::create(path)
            .with_context(|| format!("creating journal at {}", path.display()))?;
        let tx = db.begin_write()?;
        {
            for name in tables(runes) {
                tx.open_table(table(name))?;
            }
            let mut meta = tx.open_table(table(cf::META))?;
            meta.insert(META_CHAIN, chain.to_string().as_bytes())?;
            meta.insert(META_SCHEMA, SCHEMA_VERSION.to_be_bytes().as_slice())?;
            meta.insert(META_RUNES, [u8::from(runes)].as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Begins a write transaction that commits with `Durability::Immediate`
    /// and quick repair, so a reopen after a crash skips the full-file walk.
    pub fn begin_write(&self) -> Result<WriteTransaction> {
        let mut tx = self.db.begin_write()?;
        tx.set_durability(Durability::Immediate)?;
        tx.set_quick_repair(true);
        Ok(tx)
    }

    /// Highest finalized height, if any block has been finalized.
    pub fn final_tip(&self) -> Result<Option<u32>> {
        self.meta_u32(META_FINAL_TIP, "final_tip")
    }

    /// Highest height the gate has ever been opened to, so ord's committed
    /// height never exceeds it while this journal logs its events.
    pub fn gate_limit(&self) -> Result<Option<u32>> {
        self.meta_u32(META_GATE_LIMIT, "gate_limit")
    }

    /// Durably records `height` as the gate limit, before the gate opens to
    /// it.
    pub fn set_gate_limit(&self, height: u32) -> Result<()> {
        let tx = self.begin_write()?;
        tx.open_table(table(cf::META))?
            .insert(META_GATE_LIMIT, height.to_be_bytes().as_slice())?;
        tx.commit()?;
        Ok(())
    }

    fn meta_u32(&self, key: &[u8], name: &str) -> Result<Option<u32>> {
        meta_u32_in(&self.db.begin_read()?, key, name)
    }

    /// The chain this journal belongs to.
    pub fn chain(&self) -> Result<Chain> {
        let tx = self.db.begin_read()?;
        let meta = tx.open_table(table(cf::META))?;
        let raw = meta.get(META_CHAIN)?.context("journal meta has no chain")?;
        let name = std::str::from_utf8(raw.value()).context("journal chain is not UTF-8")?;
        name.parse()
            .map_err(|err| anyhow!("journal chain {name} is unknown: {err}"))
    }

    /// Whether this open created the journal's meta, so the journal held no
    /// data before it.
    pub fn created(&self) -> bool {
        self.created
    }

    /// Whether the journal keeps rune records.
    pub fn runes(&self) -> bool {
        self.runes
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }
}

/// Reads the 4-byte big-endian meta value `key` inside `tx`.
fn meta_u32_in(tx: &redb::ReadTransaction, key: &[u8], name: &str) -> Result<Option<u32>> {
    let meta = tx.open_table(table(cf::META))?;
    let Some(raw) = meta.get(key)? else {
        return Ok(None);
    };
    let raw = raw.value();
    let bytes: [u8; 4] = raw
        .try_into()
        .map_err(|_| anyhow!("{name} is {} bytes, expected 4", raw.len()))?;
    Ok(Some(u32::from_be_bytes(bytes)))
}

/// Highest finalized height as of `tx`.
fn final_tip_in(tx: &redb::ReadTransaction) -> Result<Option<u32>> {
    meta_u32_in(tx, META_FINAL_TIP, "final_tip")
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
