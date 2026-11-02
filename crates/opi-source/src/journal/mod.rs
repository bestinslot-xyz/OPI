//! The journal: ord's raw events and the records finalize makes of them, for
//! a rolling window of recent blocks.
//!
//! A journal is a directory. Everything keyed by height lives in height
//! files (see [`files`]): `raw/` holds each staged height's raw events until
//! finalize has made its records, and `rec/` holds each finalized height's
//! records, grouped in directories of [`JournalOptions::segment`] heights so
//! a prune deletes whole directories. A height file is written once, made
//! durable by a rename, and deleted whole, so the journal's disk use is the
//! data it holds.
//!
//! `meta.json` holds the settings the journal was built with, the gate limit
//! and the pruned floor, replaced by an atomic rename. `tracked.redb` holds
//! the one fact finalize needs from before the window: which bound text and
//! JSON inscriptions have not been transferred yet (see [`tracked`]).
//! `lock` is held while the journal is open.
//!
//! The final tip is the highest height with a record file: finalize writes
//! heights in order, and a rollback deletes them from the top down.

mod files;
mod finalize;
pub mod order;
mod prune;
mod reader;
mod reorg;
mod runes;
mod schema;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tracked;

#[cfg(feature = "testing")]
pub use files::testing_free;
pub use {
    files::{DiskFull, FORMAT_VERSION, HeightFile, Rows, Stream},
    finalize::{ChainData, EmissionOrderError, FINALIZE_BATCH, finalize_range, is_json_body},
    prune::Pruned,
    reader::JournalReader,
    reorg::{reorg_check, rollback_to},
    runes::RuneModelError,
    schema::*,
};

use {
    crate::ord_settings::{enabled_or_disabled, with_or_without},
    anyhow::{Context, Result, bail, ensure},
    bitcoin::{BlockHash, hashes::Hash},
    files::Store,
    ord::Chain,
    redb::{Database, Durability, TableDefinition, WriteTransaction},
    serde::{Deserialize, Serialize},
    std::{
        fs::File,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicI64, Ordering},
        },
    },
};

/// Table names. A height table's rows live in height files (see
/// [`files::TABLES`]); `tracked.redb` names its own.
pub mod cf {
    /// Transfer records: height (`u32` BE) ++ idx (`u32` BE) -> record.
    pub const TRANSFERS: &str = "ord_transfers";
    /// The info of every inscription a height's records name: height (`u32`
    /// BE) ++ inscription id (store form) -> zstd-compressed info.
    pub const INFO: &str = "ord_inscription_info";
    /// Block headers, the head of each record file: height (`u32` BE) ->
    /// consensus-encoded header.
    pub const HEADERS: &str = "height_to_block_header";
    /// Raw events: height (`u32` BE) ++ sequence (`u32` BE) -> encoded
    /// event.
    pub const RAW_EVENTS: &str = "raw_events";
    /// The hash each height's raw events were logged under, the head of
    /// each raw file: height (`u32` BE) -> block hash.
    pub const RAW_HEIGHTS: &str = "raw_heights";
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

/// `meta.json`'s file name inside the journal dir.
pub const META_FILE: &str = "meta.json";
const LOCK_FILE: &str = "lock";
/// File names of the redb journal layouts, which this layout refuses.
const REDB_META_FILE: &str = "meta.redb";
const SINGLE_FILE: &str = "journal.redb";

const SCHEMA_VERSION: u32 = 3;
const ZSTD_LEVEL: i32 = 3;

/// The heights per segment `OPI_JOURNAL_SEGMENT` accepts.
pub const SEGMENT_RANGE: std::ops::RangeInclusive<u32> = 100..=100_000;
/// Heights per segment unless configured.
pub const DEFAULT_SEGMENT: u32 = 1000;

/// `tracked.redb`'s page cache unless configured.
pub const DEFAULT_CACHE_BYTES: usize = 1024 * 1024 * 1024;

/// The free disk space, in GiB, `OPI_MIN_FREE_DISK_GB` accepts.
pub const MIN_FREE_GB_RANGE: std::ops::RangeInclusive<u64> = 1..=10_000;
/// The free disk space, in GiB, the journal keeps unless configured.
pub const DEFAULT_MIN_FREE_GB: u64 = 20;

/// How a journal is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalOptions {
    /// `tracked.redb`'s page cache in bytes. Height files are read through
    /// the kernel's page cache.
    pub cache_bytes: usize,
    /// Whether the journal keeps every inscription move, for the
    /// inscriptions module. Fixed when the journal is created.
    pub inscriptions: bool,
    /// Heights per segment directory. Fixed when the journal is created.
    pub segment: u32,
    /// Free bytes a write must leave on the journal's file system; a write
    /// that would leave less fails with [`DiskFull`]. 0 for tools and tests;
    /// `opi-indexer` sets it from `OPI_MIN_FREE_DISK_GB`.
    pub min_free_bytes: u64,
}

impl Default for JournalOptions {
    fn default() -> Self {
        JournalOptions {
            cache_bytes: DEFAULT_CACHE_BYTES,
            inscriptions: false,
            segment: DEFAULT_SEGMENT,
            min_free_bytes: 0,
        }
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
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", parent.display()))
}

/// Writes `bytes` to `path` through `<path>.init`, synced and renamed into
/// place, so a crash leaves the old file or the new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let init = init_path(path);
    std::fs::write(&init, bytes).with_context(|| format!("writing {}", init.display()))?;
    File::open(&init)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("syncing {}", init.display()))?;
    std::fs::rename(&init, path)
        .with_context(|| format!("renaming {} into place", init.display()))?;
    sync_parent(path)
}

/// Definition of the redb table called `name`.
pub fn table(name: &'static str) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
    TableDefinition::new(name)
}

/// Another open holds the journal: its `lock` is taken.
#[derive(Debug)]
pub struct InUse(pub PathBuf);

impl std::fmt::Display for InUse {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "the journal at {} is open elsewhere", self.0.display())
    }
}

impl std::error::Error for InUse {}

/// Whether `err` reports a journal another open holds.
pub fn is_in_use(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.is::<InUse>())
}

/// `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Meta {
    schema: u32,
    chain: String,
    runes: bool,
    inscriptions: bool,
    segment: u32,
    /// Highest height the gate has ever been opened to.
    gate_limit: Option<u32>,
    /// Lowest height the journal serves records of; raised before a prune
    /// deletes record files.
    pruned_below: Option<u32>,
}

/// The journal in a directory. Clones share it.
#[derive(Clone)]
pub struct Journal {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    meta: Mutex<Meta>,
    store: Store,
    tracked: Database,
    options: JournalOptions,
    /// Highest finalized height, -1 when none.
    final_tip: AtomicI64,
    chain: Chain,
    created: bool,
    runes: bool,
    inscriptions: bool,
    _lock: File,
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
    /// `inscriptions` or segment setting, or `dir` holds a redb journal. A
    /// refused open leaves the journal unchanged.
    ///
    /// The journal holds an exclusive lock on its `lock` file while open, so
    /// a second open of the same journal fails with [`InUse`].
    pub fn open_with(
        dir: &Path,
        chain: Chain,
        runes: bool,
        options: &JournalOptions,
    ) -> Result<Journal> {
        ensure!(options.segment > 0, "a journal segment holds no heights");
        refuse_redb_layout(dir)?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let lock = lock(dir)?;

        let meta_path = dir.join(META_FILE);
        remove_if_exists(&init_path(&meta_path))?;
        let created = !meta_path.exists();
        let meta = if created {
            let meta = Meta {
                schema: SCHEMA_VERSION,
                chain: chain.to_string(),
                runes,
                inscriptions: options.inscriptions,
                segment: options.segment,
                gate_limit: None,
                pruned_below: None,
            };
            write_atomically(&meta_path, &serde_json::to_vec_pretty(&meta)?)?;
            meta
        } else {
            let raw = std::fs::read(&meta_path)
                .with_context(|| format!("reading {}", meta_path.display()))?;
            serde_json::from_slice(&raw)
                .with_context(|| format!("parsing {}", meta_path.display()))?
        };
        check_meta(&meta, dir, chain, runes, options)?;

        let tracked = tracked::open(&dir.join(tracked::TRACKED_FILE), options.cache_bytes)?;
        let store = Store::new(dir, options.segment, options.min_free_bytes)?;
        let journal = Journal {
            inner: Arc::new(Inner {
                dir: dir.to_path_buf(),
                meta: Mutex::new(meta),
                store,
                tracked,
                options: *options,
                final_tip: AtomicI64::new(-1),
                chain,
                created,
                runes,
                inscriptions: options.inscriptions,
                _lock: lock,
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

    pub(crate) fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Highest finalized height, if any block has been finalized.
    pub fn final_tip(&self) -> Result<Option<u32>> {
        let tip = self.inner.final_tip.load(Ordering::SeqCst);
        Ok(u32::try_from(tip).ok())
    }

    /// Reads the final tip from the record files: the highest height with
    /// one.
    fn load_final_tip(&self) -> Result<()> {
        let tip = self.store().last_height(Stream::Records)?;
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
        Ok(self.inner.meta.lock().unwrap().gate_limit)
    }

    /// Durably records `height` as the gate limit, before the gate opens to
    /// it.
    pub fn set_gate_limit(&self, height: u32) -> Result<()> {
        self.update_meta(|meta| meta.gate_limit = Some(height))
    }

    /// Applies `change` to `meta.json` and writes it durably.
    fn update_meta(&self, change: impl FnOnce(&mut Meta)) -> Result<()> {
        let mut meta = self.inner.meta.lock().unwrap();
        let mut next = meta.clone();
        change(&mut next);
        write_atomically(
            &self.dir().join(META_FILE),
            &serde_json::to_vec_pretty(&next)?,
        )?;
        *meta = next;
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

    /// Lowest height whose records the journal serves; 0 when nothing was
    /// pruned.
    pub fn pruned_below(&self) -> Result<u32> {
        Ok(self.inner.meta.lock().unwrap().pruned_below.unwrap_or(0))
    }

    /// The first height of every record segment, ascending.
    pub fn segment_starts(&self) -> Result<Vec<u32>> {
        self.store().segment_starts(Stream::Records)
    }

    /// The record file of `height`, `None` when it has none.
    pub fn records(&self, height: u32) -> Result<Option<HeightFile>> {
        self.store().read(Stream::Records, height)
    }

    /// Writes `file` as the records of the next unfinalized height and makes
    /// it the final tip.
    pub fn append_records(&self, file: &HeightFile) -> Result<()> {
        let expected = self.final_tip()?.map_or(0, |tip| tip + 1);
        ensure!(
            file.height == expected,
            "appending the records of height {}, but the next unfinalized height is {expected}",
            file.height
        );
        self.store().write(Stream::Records, file)?;
        self.set_cached_tip(Some(file.height));
        Ok(())
    }

    /// The raw events of `height` logged under `hash`, in emission order;
    /// none when the height has no raw file or one logged under another
    /// hash.
    pub fn raw_rows(&self, height: u32, hash: BlockHash) -> Result<Option<HeightFile>> {
        let store = self.store();
        if store
            .read_head(Stream::Raw, height)?
            .is_none_or(|head| head != hash.as_byte_array())
        {
            return Ok(None);
        }
        store.read(Stream::Raw, height)
    }

    /// Writes the raw events of `height`, logged under `hash`, replacing any.
    pub fn write_raw(&self, height: u32, hash: BlockHash, events: &[Vec<u8>]) -> Result<()> {
        let mut file = HeightFile::new(height, hash.as_byte_array().to_vec());
        let rows = file.rows_mut(cf::RAW_EVENTS);
        for (seq, event) in events.iter().enumerate() {
            rows.insert(&raw_event_key(height, u32::try_from(seq)?), event)?;
        }
        self.store().write(Stream::Raw, &file)
    }

    /// Highest height with raw events.
    pub fn last_raw_height(&self) -> Result<Option<u32>> {
        self.store().last_height(Stream::Raw)
    }

    /// Deletes the raw events above `height`, all of them when `None`.
    pub fn remove_raw_above(&self, height: Option<u32>) -> Result<()> {
        self.store().remove_above(Stream::Raw, height)
    }

    /// Every row of table `name` in `stream`, height by height in key order:
    /// a body table's rows, or for [`cf::HEADERS`] and [`cf::RAW_HEIGHTS`]
    /// each file's head under its height. For tests and tools.
    pub fn table_rows(&self, stream: Stream, name: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let head = matches!(
            (stream, name),
            (Stream::Records, cf::HEADERS) | (Stream::Raw, cf::RAW_HEIGHTS)
        );
        let mut rows = Vec::new();
        for height in self.heights(stream)? {
            if let Some(file) = self.store().read(stream, height)? {
                if head {
                    rows.push((height.to_be_bytes().to_vec(), file.head));
                } else {
                    rows.extend(file.rows(name).map(|(k, v)| (k.to_vec(), v.to_vec())));
                }
            }
        }
        Ok(rows)
    }

    /// The heights with a file in `stream`, ascending.
    pub fn heights(&self, stream: Stream) -> Result<Vec<u32>> {
        let store = self.store();
        let mut heights = Vec::new();
        for start in store.segment_starts(stream)? {
            heights.extend(store.heights_in(stream, start)?);
        }
        Ok(heights)
    }

    /// The path of `height`'s file in `stream`.
    pub fn height_path(&self, stream: Stream, height: u32) -> PathBuf {
        self.store().path(stream, height)
    }

    /// Deletes the files of the journal in `dir`, which no open may hold.
    pub fn remove_files(dir: &Path) -> Result<()> {
        for name in [META_FILE, LOCK_FILE, tracked::TRACKED_FILE] {
            let path = dir.join(name);
            remove_if_exists(&path)?;
            remove_if_exists(&init_path(&path))?;
        }
        for stream in Stream::ALL {
            match std::fs::remove_dir_all(dir.join(stream.dir_name())) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }
}

/// Refuses a directory holding a journal of a redb layout.
fn refuse_redb_layout(dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name == SINGLE_FILE
            || name == REDB_META_FILE
            || (name.starts_with("seg-") && name.ends_with(".redb"))
        {
            bail!(
                "{} holds a redb journal ({name}), which this version does not read; delete the \
                 journal and ord's data dir and index again, or run the release that built it",
                dir.display()
            );
        }
    }
    Ok(())
}

/// Takes the exclusive lock of the journal in `dir`.
fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK_FILE);
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(InUse(dir.to_path_buf()).into()),
        Err(err) => Err(err).with_context(|| format!("locking {}", path.display())),
    }
}

/// Checks `meta.json` against the settings the journal is opened with.
fn check_meta(
    meta: &Meta,
    dir: &Path,
    chain: Chain,
    runes: bool,
    options: &JournalOptions,
) -> Result<()> {
    if meta.schema != SCHEMA_VERSION {
        bail!(
            "unsupported journal schema {}, expected {SCHEMA_VERSION}",
            meta.schema
        );
    }
    if meta.chain != chain.to_string() {
        bail!(
            "journal chain mismatch: journal is {}, requested {chain}",
            meta.chain
        );
    }
    if meta.runes != runes {
        bail!(
            "journal at {} was built {} rune indexing, but runes is {}; start with runes {} \
             again, or delete the journal and ord's data dir and index again",
            dir.display(),
            with_or_without(meta.runes),
            enabled_or_disabled(runes),
            enabled_or_disabled(meta.runes),
        );
    }
    if meta.inscriptions != options.inscriptions {
        bail!(
            "journal at {} was built {} the inscription moves the inscriptions module \
             reads, but the module is {}; start with it {} again, or delete the journal \
             and ord's data dir and index again",
            dir.display(),
            with_or_without(meta.inscriptions),
            enabled_or_disabled(options.inscriptions),
            enabled_or_disabled(meta.inscriptions),
        );
    }
    if meta.segment != options.segment {
        bail!(
            "journal at {} was built with segments of {} heights, but \
             OPI_JOURNAL_SEGMENT is {}; set it to {} again, or delete the journal and \
             ord's data dir and index again",
            dir.display(),
            meta.segment,
            options.segment,
            meta.segment,
        );
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
