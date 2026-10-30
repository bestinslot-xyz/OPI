//! The journal: ord's raw event log and the records finalize makes of it,
//! for a rolling window of recent blocks.
//!
//! A journal is a directory. Every table keyed by height lives in segment
//! files of [`JournalOptions::segment`] consecutive heights each,
//! `seg-<first height>.redb`, so a prune deletes whole files and the
//! journal's disk use follows from the window it keeps. `meta.redb` holds
//! the settings the journal was built with and the gate limit.
//! `tracked.redb` holds the one fact finalize needs from before the window:
//! which bound text and JSON inscriptions have not been transferred yet (see
//! [`tracked`]).
//!
//! A write touches one file at a time and commits on its own. Each segment
//! records the highest finalized height it holds, so a finalize or rollback
//! that spans two segments leaves a consistent final tip after a crash at
//! any point: the final tip is the highest one any segment records.

mod finalize;
pub mod order;
mod prune;
mod reader;
mod reorg;
mod runes;
mod schema;
mod segments;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tracked;

pub use {
    finalize::{ChainData, EmissionOrderError, FINALIZE_BATCH, finalize_range, is_json_body},
    prune::Pruned,
    reader::JournalReader,
    reorg::{reorg_check, rollback_to},
    runes::RuneModelError,
    schema::*,
    segments::{SEGMENT_PREFIX, segment_name},
};

use {
    crate::ord_settings::{enabled_or_disabled, with_or_without},
    anyhow::{Context, Result, anyhow, bail, ensure},
    ord::Chain,
    redb::{
        Database, Durability, ReadTransaction, ReadableDatabase, TableDefinition, WriteTransaction,
    },
    segments::Segments,
    std::{
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicI64, Ordering},
        },
    },
};

/// Table names. Each names one redb table mapping `&[u8]` to `&[u8]`.
pub mod cf {
    /// Transfer records: height (`u32` BE) ++ idx (`u32` BE) -> record.
    pub const TRANSFERS: &str = "ord_transfers";
    /// The info of every inscription a height's records name: height (`u32`
    /// BE) ++ inscription id (store form) -> zstd-compressed info.
    pub const INFO: &str = "ord_inscription_info";
    pub const HEADERS: &str = "height_to_block_header";
    pub const RAW_EVENTS: &str = "raw_events";
    pub const RAW_HEIGHTS: &str = "raw_heights";
    /// `meta.redb`'s settings, and each segment's final tip.
    pub const META: &str = "meta";
    /// Rune events but the inputs: height (`u32` BE) ++ sequence (`u32` BE)
    /// -> encoded event.
    pub const RUNE_EVENTS: &str = "rune_events";
    /// Rune etchings: height (`u32` BE) ++ sequence (`u32` BE) -> encoded
    /// etching.
    pub const RUNE_ETCHINGS: &str = "rune_etchings";
    /// The outpoints each transaction with rune events spends: height
    /// (`u32` BE) ++ sequence (`u32` BE) -> encoded spend.
    pub const RUNE_SPENT: &str = "rune_spent";
    /// Every inscription creation and transfer: height (`u32` BE) ++
    /// position among the height's inscription events (`u32` BE) -> encoded
    /// move.
    pub const INSCRIPTION_MOVES: &str = "inscription_moves";
}

/// The tables of a segment file, every one keyed by height first.
const SEGMENT_TABLES: [&str; 10] = [
    cf::TRANSFERS,
    cf::INFO,
    cf::HEADERS,
    cf::RAW_EVENTS,
    cf::RAW_HEIGHTS,
    cf::META,
    cf::RUNE_EVENTS,
    cf::RUNE_ETCHINGS,
    cf::RUNE_SPENT,
    cf::INSCRIPTION_MOVES,
];

/// The record tables of a segment: what finalize writes and a rollback
/// deletes.
const RECORD_TABLES: [&str; 7] = [
    cf::TRANSFERS,
    cf::INFO,
    cf::HEADERS,
    cf::RUNE_EVENTS,
    cf::RUNE_ETCHINGS,
    cf::RUNE_SPENT,
    cf::INSCRIPTION_MOVES,
];

/// `meta.redb`'s file name inside the journal dir.
pub const META_FILE: &str = "meta.redb";
/// The file name of the single-file journal layout, which this layout
/// refuses.
pub const LEGACY_FILE: &str = "journal.redb";

const META_GATE_LIMIT: &[u8] = b"gate_limit";
const META_CHAIN: &[u8] = b"chain";
const META_SCHEMA: &[u8] = b"schema";
/// Whether the journal keeps rune records: one byte, 1 or 0.
const META_RUNES: &[u8] = b"runes";
/// Whether the journal keeps every inscription move: one byte, 1 or 0.
const META_INSCRIPTIONS: &[u8] = b"inscriptions";
/// Heights per segment (`u32` BE), fixed when the journal is created.
const META_SEGMENT: &[u8] = b"segment";
/// Lowest height the journal serves records of (`u32` BE); absent when
/// nothing was pruned. Raised before a prune deletes segment files.
const META_PRUNED_BELOW: &[u8] = b"pruned_below";
/// A segment's highest finalized height (`u32` BE), in its own meta table.
const SEGMENT_FINAL_TIP: &[u8] = b"final_tip";
const SCHEMA_VERSION: u32 = 2;
const ZSTD_LEVEL: i32 = 3;

/// The heights per segment `OPI_JOURNAL_SEGMENT` accepts.
pub const SEGMENT_RANGE: std::ops::RangeInclusive<u32> = 100..=100_000;
/// Heights per segment unless configured.
pub const DEFAULT_SEGMENT: u32 = 1000;

/// redb's own default page cache size.
pub const DEFAULT_CACHE_BYTES: usize = 1024 * 1024 * 1024;

/// How a journal is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalOptions {
    /// redb page cache in bytes, shared by the journal's files: a quarter
    /// goes to `tracked.redb` and the rest is split evenly among the
    /// segments open at once (see [`segments::OPEN_SEGMENTS`]).
    pub cache_bytes: usize,
    /// Whether the journal keeps every inscription move, for the
    /// inscriptions module. Fixed when the journal is created.
    pub inscriptions: bool,
    /// Heights per segment file. Fixed when the journal is created.
    pub segment: u32,
}

impl Default for JournalOptions {
    fn default() -> Self {
        JournalOptions {
            cache_bytes: DEFAULT_CACHE_BYTES,
            inscriptions: false,
            segment: DEFAULT_SEGMENT,
        }
    }
}

impl JournalOptions {
    /// The page cache of `tracked.redb`.
    fn tracked_cache(&self) -> usize {
        self.cache_bytes / 4
    }

    /// The page cache of each open segment.
    fn segment_cache(&self) -> usize {
        (self.cache_bytes - self.tracked_cache()) / segments::OPEN_SEGMENTS
    }
}

/// A redb database with a page cache of `cache_bytes`, created when absent.
fn create_db(path: &Path, cache_bytes: usize) -> Result<Database> {
    let mut builder = Database::builder();
    builder.set_cache_size(cache_bytes);
    builder
        .create(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Begins a write transaction on `db` that commits with
/// `Durability::Immediate` and quick repair, so a reopen after a crash skips
/// the full-file walk.
pub fn begin_write(db: &Database) -> Result<WriteTransaction> {
    let mut tx = db.begin_write()?;
    tx.set_durability(Durability::Immediate)?;
    tx.set_quick_repair(true);
    Ok(tx)
}

/// The scratch path a file is built at before being renamed to `path`.
pub fn init_path(path: &Path) -> PathBuf {
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

/// Builds a database at `path` through `build`, at `<path>.init` first and
/// renamed into place once complete, so a crash leaves no half-made file.
fn create_atomically(
    path: &Path,
    cache_bytes: usize,
    build: impl FnOnce(&Database) -> Result<()>,
) -> Result<()> {
    let init = init_path(path);
    remove_if_exists(&init)?;
    {
        let db = create_db(&init, cache_bytes)?;
        build(&db)?;
    }
    std::fs::rename(&init, path)
        .with_context(|| format!("renaming {} into place", init.display()))?;
    sync_parent(path)
}

/// Whether `err` reports a journal another open holds: redb locks each file
/// exclusively while a database is open.
pub fn is_in_use(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<redb::DatabaseError>(),
            Some(redb::DatabaseError::DatabaseAlreadyOpen)
        ) || matches!(
            cause.downcast_ref::<redb::Error>(),
            Some(redb::Error::DatabaseAlreadyOpen)
        )
    })
}

/// Definition of the journal table called `name`.
pub fn table(name: &'static str) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
    TableDefinition::new(name)
}

/// The journal in a directory. Clones share it.
#[derive(Clone)]
pub struct Journal {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    meta: Database,
    tracked: Database,
    segments: Mutex<Segments>,
    options: JournalOptions,
    /// Highest finalized height, -1 when none.
    final_tip: AtomicI64,
    chain: Chain,
    created: bool,
    runes: bool,
    inscriptions: bool,
}

impl Journal {
    /// [`Journal::open_with`] with the default options, for tests and tools.
    pub fn open(dir: &Path, chain: Chain, runes: bool) -> Result<Journal> {
        Journal::open_with(dir, chain, runes, &JournalOptions::default())
    }

    /// Opens the journal in `dir`, creating it for `chain` when `dir` holds
    /// none, with rune records when `runes` is set and every inscription
    /// move when `options.inscriptions` is set. Fails when the journal
    /// belongs to another chain or schema, was built with another `runes`,
    /// `inscriptions` or segment setting, or `dir` holds a single-file
    /// journal. A refused open leaves the journal unchanged.
    ///
    /// redb locks each open file, so a second open of the same journal from
    /// another process fails.
    pub fn open_with(
        dir: &Path,
        chain: Chain,
        runes: bool,
        options: &JournalOptions,
    ) -> Result<Journal> {
        ensure!(options.segment > 0, "a journal segment holds no heights");
        let legacy = dir.join(LEGACY_FILE);
        if legacy.exists() {
            bail!(
                "{} is a single-file journal, which this version no longer reads; delete it and \
                 ord's data dir and index again, or run the release that built it",
                legacy.display()
            );
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let meta_path = dir.join(META_FILE);
        let created = !meta_path.exists();
        if created {
            create_atomically(&meta_path, 1 << 20, |db| {
                let tx = begin_write(db)?;
                {
                    let mut meta = tx.open_table(table(cf::META))?;
                    meta.insert(META_CHAIN, chain.to_string().as_bytes())?;
                    meta.insert(META_SCHEMA, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                    meta.insert(META_RUNES, [u8::from(runes)].as_slice())?;
                    meta.insert(
                        META_INSCRIPTIONS,
                        [u8::from(options.inscriptions)].as_slice(),
                    )?;
                    meta.insert(META_SEGMENT, options.segment.to_be_bytes().as_slice())?;
                }
                tx.commit()?;
                Ok(())
            })?;
        }
        let meta = create_db(&meta_path, 1 << 20)?;
        check_meta(&meta, dir, chain, runes, options)?;

        let tracked = tracked::open(&dir.join(tracked::TRACKED_FILE), options.tracked_cache())?;
        let segments = Segments::scan(dir, options)?;
        let journal = Journal {
            inner: Arc::new(Inner {
                dir: dir.to_path_buf(),
                meta,
                tracked,
                segments: Mutex::new(segments),
                options: *options,
                final_tip: AtomicI64::new(-1),
                chain,
                created,
                runes,
                inscriptions: options.inscriptions,
            }),
        };
        journal.load_final_tip()?;
        Ok(journal)
    }

    /// The journal's directory.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn options(&self) -> &JournalOptions {
        &self.inner.options
    }

    /// Heights per segment.
    pub fn segment_len(&self) -> u32 {
        self.inner.options.segment
    }

    /// First height of the segment holding `height`.
    pub fn segment_start(&self, height: u32) -> u32 {
        height - height % self.segment_len()
    }

    /// Highest finalized height, if any block has been finalized.
    pub fn final_tip(&self) -> Result<Option<u32>> {
        let tip = self.inner.final_tip.load(Ordering::SeqCst);
        Ok(u32::try_from(tip).ok())
    }

    /// Reads the final tip from the segments: the highest one any of them
    /// records.
    fn load_final_tip(&self) -> Result<()> {
        let mut tip = None;
        for start in self.segment_starts().into_iter().rev() {
            if let Some(db) = self.segment(start)?
                && let Some(segment_tip) = segment_tip_in(&db.begin_read()?)?
            {
                tip = Some(segment_tip);
                break;
            }
        }
        self.set_cached_tip(tip);
        Ok(())
    }

    fn set_cached_tip(&self, tip: Option<u32>) {
        self.inner
            .final_tip
            .store(tip.map_or(-1, i64::from), Ordering::SeqCst);
    }

    /// Highest height the gate has ever been opened to, so ord's committed
    /// height never exceeds it while this journal logs its events.
    pub fn gate_limit(&self) -> Result<Option<u32>> {
        meta_u32_in(
            &self.inner.meta.begin_read()?,
            META_GATE_LIMIT,
            "gate_limit",
        )
    }

    /// Durably records `height` as the gate limit, before the gate opens to
    /// it.
    pub fn set_gate_limit(&self, height: u32) -> Result<()> {
        self.set_meta_u32(META_GATE_LIMIT, height)
    }

    fn set_meta_u32(&self, key: &[u8], value: u32) -> Result<()> {
        let tx = begin_write(&self.inner.meta)?;
        tx.open_table(table(cf::META))?
            .insert(key, value.to_be_bytes().as_slice())?;
        tx.commit()?;
        Ok(())
    }

    /// The chain this journal belongs to.
    pub fn chain(&self) -> Result<Chain> {
        Ok(self.inner.chain)
    }

    /// Whether this open created the journal, so it held no data before it.
    pub fn created(&self) -> bool {
        self.inner.created
    }

    /// Whether the journal keeps rune records.
    pub fn runes(&self) -> bool {
        self.inner.runes
    }

    /// Whether the journal keeps every inscription move.
    pub fn inscriptions(&self) -> bool {
        self.inner.inscriptions
    }

    /// `tracked.redb`.
    pub fn tracked_db(&self) -> &Database {
        &self.inner.tracked
    }

    /// The first height of every segment file, ascending.
    pub fn segment_starts(&self) -> Vec<u32> {
        self.inner.segments.lock().unwrap().starts()
    }

    /// The segment holding `height`, `None` when its file does not exist.
    /// The handle stays valid while held, even after a prune deletes the
    /// file.
    pub fn segment(&self, height: u32) -> Result<Option<Arc<Database>>> {
        let start = self.segment_start(height);
        self.inner.segments.lock().unwrap().get(start)
    }

    /// The segment holding `height`, created when absent.
    pub fn segment_mut(&self, height: u32) -> Result<Arc<Database>> {
        let start = self.segment_start(height);
        self.inner.segments.lock().unwrap().get_or_create(start)
    }

    /// Deletes the files of the journal in `dir`, which no open may hold.
    pub fn remove_files(dir: &Path) -> Result<()> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let base = name.strip_suffix(".init").unwrap_or(&name);
            if base == META_FILE
                || base == tracked::TRACKED_FILE
                || (base.starts_with(SEGMENT_PREFIX) && base.ends_with(".redb"))
            {
                remove_if_exists(&entry.path())?;
            }
        }
        Ok(())
    }

    /// Lowest height whose records the journal serves; 0 when nothing was
    /// pruned.
    pub fn pruned_below(&self) -> Result<u32> {
        Ok(meta_u32_in(
            &self.inner.meta.begin_read()?,
            META_PRUNED_BELOW,
            "pruned_below",
        )?
        .unwrap_or(0))
    }
}

/// Checks `meta.redb` against the settings the journal is opened with.
fn check_meta(
    db: &Database,
    dir: &Path,
    chain: Chain,
    runes: bool,
    options: &JournalOptions,
) -> Result<()> {
    let tx = db.begin_read()?;
    let meta = tx.open_table(table(cf::META))?;
    let get = |key: &[u8]| -> Result<Vec<u8>> {
        Ok(meta
            .get(key)?
            .with_context(|| {
                format!(
                    "journal meta lacks {}",
                    String::from_utf8_lossy(key).into_owned()
                )
            })?
            .value()
            .to_vec())
    };
    if get(META_SCHEMA)? != SCHEMA_VERSION.to_be_bytes() {
        bail!("unsupported journal schema, expected {SCHEMA_VERSION}");
    }
    let existing = get(META_CHAIN)?;
    if existing != chain.to_string().as_bytes() {
        bail!(
            "journal chain mismatch: journal is {}, requested {chain}",
            String::from_utf8_lossy(&existing)
        );
    }
    let flag = |key: &[u8], name: &str| -> Result<bool> {
        match get(key)?.as_slice() {
            [0] => Ok(false),
            [1] => Ok(true),
            other => bail!("journal {name} setting is {other:?}, expected [0] or [1]"),
        }
    };
    let built = flag(META_RUNES, "runes")?;
    if built != runes {
        bail!(
            "journal at {} was built {} rune indexing, but runes is {}; start with runes {} \
             again, or delete the journal and ord's data dir and index again",
            dir.display(),
            with_or_without(built),
            enabled_or_disabled(runes),
            enabled_or_disabled(built),
        );
    }
    let built = flag(META_INSCRIPTIONS, "inscriptions")?;
    if built != options.inscriptions {
        bail!(
            "journal at {} was built {} the inscription moves the inscriptions module \
             reads, but the module is {}; start with it {} again, or delete the journal \
             and ord's data dir and index again",
            dir.display(),
            with_or_without(built),
            enabled_or_disabled(options.inscriptions),
            enabled_or_disabled(built),
        );
    }
    let segment = u32::from_be_bytes(
        get(META_SEGMENT)?
            .try_into()
            .map_err(|_| anyhow!("journal segment setting is not 4 bytes"))?,
    );
    if segment != options.segment {
        bail!(
            "journal at {} was built with segments of {segment} heights, but \
             OPI_JOURNAL_SEGMENT is {}; set it to {segment} again, or delete the journal and \
             ord's data dir and index again",
            dir.display(),
            options.segment
        );
    }
    Ok(())
}

/// Reads the 4-byte big-endian meta value `key` inside `tx`.
fn meta_u32_in(tx: &ReadTransaction, key: &[u8], name: &str) -> Result<Option<u32>> {
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

/// Highest finalized height a segment holds, as of `tx`.
fn segment_tip_in(tx: &ReadTransaction) -> Result<Option<u32>> {
    meta_u32_in(tx, SEGMENT_FINAL_TIP, "final_tip")
}

/// Sets a segment's final tip to `height`, or clears it when `None`.
fn set_segment_tip(tx: &WriteTransaction, height: Option<u32>) -> Result<()> {
    let mut meta = tx.open_table(table(cf::META))?;
    match height {
        Some(height) => {
            meta.insert(SEGMENT_FINAL_TIP, height.to_be_bytes().as_slice())?;
        }
        None => {
            meta.remove(SEGMENT_FINAL_TIP)?;
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

/// Deletes every row of `tx`'s record tables from `height` up.
fn delete_records_from(tx: &WriteTransaction, height: u32) -> Result<()> {
    let first = height.to_be_bytes();
    for name in RECORD_TABLES {
        tx.open_table(table(name))?
            .retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    }
    Ok(())
}
