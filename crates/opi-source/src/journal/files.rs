//! Height files: the journal's store for everything keyed by height.
//!
//! Each height has up to two files, one per [`Stream`]: its raw events,
//! staged as ord indexes the block, and its records, written when finalize
//! makes them. They are separate files because the stager writes raw events
//! ahead of finalize, and a replay of ord's replaces them without touching
//! records.
//!
//! A file lives at `<stream>/<segment start>/<height>`, both numbers nine
//! digits wide. A segment groups [`super::JournalOptions::segment`]
//! consecutive heights into one directory, so a prune deletes directories.
//!
//! A write builds `<height>.tmp`, syncs its data, renames it into place and
//! syncs the directory, so a height's file exists complete or not at all, and
//! files of one stream become durable in the order they are written. A crash
//! leaves at most a `.tmp` file, which [`Store::new`] deletes. A rollback
//! deletes from the highest height down, so a crash during it leaves the
//! stream a prefix of what it held.
//!
//! # File layout (format 1)
//!
//! Little-endian. A 28-byte header, the head, then the body:
//!
//! | bytes | field |
//! |---|---|
//! | 4 | magic `OPIJ` |
//! | 1 | format version, [`FORMAT_VERSION`] |
//! | 1 | stream: 0 raw, 1 records |
//! | 2 | flags: bit 0 set when the body is zstd-compressed |
//! | 4 | height |
//! | 4 | head length |
//! | 4 | body length, as stored |
//! | 4 | CRC-32 of the head |
//! | 4 | CRC-32 of the body, as stored |
//!
//! The head is what readers ask of a height without its rows: for raw
//! events the hash of the block ord indexed (32 bytes), for records the
//! block header (80 bytes, consensus encoding). The body holds the height's
//! rows, table by table in [`TABLES`] order, tables without rows left out:
//! the table's id (1 byte, its position in [`TABLES`]), its row count
//! (`u32`), then each row in key order as its key without the leading
//! height (`u32` length ++ bytes) and its value (`u32` length ++ bytes).
//! Keys and values are the byte layouts of [`super::schema`], so the records
//! modules hash are those the fork writes.
//!
//! Readers read and verify a whole file in one go. A prune unlinks files
//! that readers may hold open; POSIX keeps an unlinked file readable through
//! its open handles, so readers keep no reference counts.

use {
    super::{cf, remove_if_exists, sync_parent},
    anyhow::{Context, Result, bail, ensure},
    std::{
        collections::BTreeMap,
        fs::File,
        io::{ErrorKind, Read, Write},
        path::{Path, PathBuf},
    },
};

/// The version of the file layout.
pub const FORMAT_VERSION: u8 = 1;
const MAGIC: &[u8; 4] = b"OPIJ";
const HEADER_LEN: usize = 28;
const COMPRESSED: u16 = 1;
const TMP_SUFFIX: &str = ".tmp";
/// zstd level of compressed bodies: fast, and most of the gain.
const BODY_ZSTD_LEVEL: i32 = 1;

/// The tables a height file's body holds rows of, by table id. Every key
/// starts with the height.
pub const TABLES: [&str; 7] = [
    cf::RAW_EVENTS,
    cf::TRANSFERS,
    cf::INFO,
    cf::RUNE_EVENTS,
    cf::RUNE_ETCHINGS,
    cf::RUNE_SPENT,
    cf::INSCRIPTION_MOVES,
];

/// The two kinds of height files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The events ord emitted for a block, under the block's hash.
    Raw,
    /// The records finalize made of a block, under its header.
    Records,
}

impl Stream {
    pub const ALL: [Stream; 2] = [Stream::Raw, Stream::Records];

    /// The stream's directory name inside the journal dir.
    pub fn dir_name(self) -> &'static str {
        match self {
            Stream::Raw => "raw",
            Stream::Records => "rec",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Stream::Raw => 0,
            Stream::Records => 1,
        }
    }
}

/// Rows of one table, by full key (height first).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Rows(BTreeMap<Vec<u8>, Vec<u8>>);

impl Rows {
    /// Sets the row at `key` to `value`.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        ensure!(key.len() >= 4, "journal key is {} bytes", key.len());
        self.0.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    /// The rows in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.0.iter().map(|(k, v)| (k.as_slice(), v.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The contents of one height's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightFile {
    pub height: u32,
    pub head: Vec<u8>,
    tables: BTreeMap<u8, Rows>,
}

impl HeightFile {
    pub fn new(height: u32, head: Vec<u8>) -> HeightFile {
        HeightFile {
            height,
            head,
            tables: BTreeMap::new(),
        }
    }

    /// The rows of table `name`, created empty when absent.
    pub fn rows_mut(&mut self, name: &str) -> &mut Rows {
        self.tables.entry(table_id(name)).or_default()
    }

    /// The rows of body table `name`; none for any other name.
    pub fn rows(&self, name: &str) -> impl Iterator<Item = (&[u8], &[u8])> {
        TABLES
            .iter()
            .position(|table| *table == name)
            .and_then(|id| self.tables.get(&(id as u8)))
            .into_iter()
            .flat_map(Rows::iter)
    }

    /// The file's bytes. The body is compressed when that makes it smaller.
    pub(crate) fn encode(&self, stream: Stream) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        for (id, rows) in self.tables.iter().filter(|(_, rows)| !rows.is_empty()) {
            body.push(*id);
            body.extend(u32::try_from(rows.len())?.to_le_bytes());
            for (key, value) in rows.iter() {
                ensure!(
                    key[..4] == self.height.to_be_bytes(),
                    "row of height {} in the file of height {}",
                    u32::from_be_bytes(key[..4].try_into()?),
                    self.height
                );
                push_prefixed(&mut body, &key[4..])?;
                push_prefixed(&mut body, value)?;
            }
        }
        let mut flags = 0;
        if body.len() >= 512 {
            let compressed = zstd::bulk::compress(&body, BODY_ZSTD_LEVEL)?;
            if compressed.len() < body.len() {
                body = compressed;
                flags |= COMPRESSED;
            }
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.head.len() + body.len());
        out.extend(MAGIC);
        out.push(FORMAT_VERSION);
        out.push(stream.tag());
        out.extend(flags.to_le_bytes());
        out.extend(self.height.to_le_bytes());
        out.extend(u32::try_from(self.head.len())?.to_le_bytes());
        out.extend(u32::try_from(body.len())?.to_le_bytes());
        out.extend(crc32fast::hash(&self.head).to_le_bytes());
        out.extend(crc32fast::hash(&body).to_le_bytes());
        out.extend(&self.head);
        out.extend(body);
        Ok(out)
    }
}

/// The id of height table `name`.
fn table_id(name: &str) -> u8 {
    let id = TABLES
        .iter()
        .position(|table| *table == name)
        .unwrap_or_else(|| panic!("{name} is not a height table"));
    id as u8
}

fn push_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    out.extend(u32::try_from(bytes.len())?.to_le_bytes());
    out.extend(bytes);
    Ok(())
}

/// A parsed file header.
struct Header {
    flags: u16,
    head_len: usize,
    body_len: usize,
    head_crc: u32,
    body_crc: u32,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn parse_header(raw: &[u8], path: &Path, stream: Stream, height: u32) -> Result<Header> {
    ensure!(
        raw.len() == HEADER_LEN && &raw[..4] == MAGIC,
        "{} is not a journal height file",
        path.display()
    );
    ensure!(
        raw[4] == FORMAT_VERSION,
        "{} has format {}, expected {FORMAT_VERSION}",
        path.display(),
        raw[4]
    );
    ensure!(
        raw[5] == stream.tag() && u32_at(raw, 8) == height,
        "{} holds stream {} height {}, expected stream {} height {height}",
        path.display(),
        raw[5],
        u32_at(raw, 8),
        stream.tag()
    );
    Ok(Header {
        flags: u16::from_le_bytes([raw[6], raw[7]]),
        head_len: u32_at(raw, 12) as usize,
        body_len: u32_at(raw, 16) as usize,
        head_crc: u32_at(raw, 20),
        body_crc: u32_at(raw, 24),
    })
}

/// Why the journal refused to write: its file system is short of space.
#[derive(Debug)]
pub struct DiskFull {
    pub path: PathBuf,
    pub free: u64,
    pub needed: u64,
    pub min_free: u64,
}

impl std::fmt::Display for DiskFull {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "the journal's file system at {} has {:.1} GiB free; writing {} bytes would leave \
             less than OPI_MIN_FREE_DISK_GB ({:.1} GiB), so the journal stops writing; free \
             disk space, then start again",
            self.path.display(),
            gib(self.free),
            self.needed,
            gib(self.min_free)
        )
    }
}

impl std::error::Error for DiskFull {}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 30)
}

/// Free bytes on the file system holding `path`, as unprivileged writers
/// see it.
fn free_bytes(path: &Path) -> Result<u64> {
    #[cfg(feature = "testing")]
    if let Some(free) = testing_free::get() {
        return Ok(free);
    }
    let stat = rustix::fs::statvfs(path)
        .with_context(|| format!("reading free space of {}", path.display()))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// A free-space reading tests set in place of the file system's.
#[cfg(feature = "testing")]
pub mod testing_free {
    use std::sync::atomic::{AtomicU64, Ordering};

    static FREE: AtomicU64 = AtomicU64::new(u64::MAX);

    /// Makes the journal see `free` bytes free, or the real figure when
    /// `None`.
    pub fn set(free: Option<u64>) {
        FREE.store(free.unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    pub(super) fn get() -> Option<u64> {
        let free = FREE.load(Ordering::SeqCst);
        (free != u64::MAX).then_some(free)
    }
}

/// The height files of a journal dir.
pub struct Store {
    dir: PathBuf,
    segment: u32,
    min_free: u64,
}

impl Store {
    /// The store in `dir`, with segments of `segment` heights, refusing
    /// writes that would leave less than `min_free` bytes free. Deletes the
    /// `.tmp` files of writes a crash interrupted, and the segment dirs that
    /// leaves empty.
    pub fn new(dir: &Path, segment: u32, min_free: u64) -> Result<Store> {
        let store = Store {
            dir: dir.to_path_buf(),
            segment,
            min_free,
        };
        for stream in Stream::ALL {
            std::fs::create_dir_all(store.stream_dir(stream))?;
            for start in store.segment_starts(stream)? {
                let dir = store.segment_dir(stream, start);
                let mut empty = true;
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    if entry.file_name().to_string_lossy().ends_with(TMP_SUFFIX) {
                        remove_if_exists(&entry.path())?;
                    } else {
                        empty = false;
                    }
                }
                if empty {
                    remove_dir(&dir)?;
                }
            }
        }
        Ok(store)
    }

    pub fn segment_start(&self, height: u32) -> u32 {
        height - height % self.segment
    }

    fn stream_dir(&self, stream: Stream) -> PathBuf {
        self.dir.join(stream.dir_name())
    }

    fn segment_dir(&self, stream: Stream, start: u32) -> PathBuf {
        self.stream_dir(stream).join(format!("{start:09}"))
    }

    /// The path of `height`'s file in `stream`.
    pub fn path(&self, stream: Stream, height: u32) -> PathBuf {
        self.segment_dir(stream, self.segment_start(height))
            .join(format!("{height:09}"))
    }

    /// Writes `file` as its height's file in `stream`, replacing any, and
    /// makes it durable. Refuses with [`DiskFull`] when the write would leave
    /// the file system with less than the minimum free.
    pub fn write(&self, stream: Stream, file: &HeightFile) -> Result<()> {
        let bytes = file.encode(stream)?;
        let free = free_bytes(&self.dir)?;
        let needed = bytes.len() as u64;
        if free < needed.saturating_add(self.min_free) {
            return Err(DiskFull {
                path: self.dir.clone(),
                free,
                needed,
                min_free: self.min_free,
            }
            .into());
        }
        let path = self.path(stream, file.height);
        let dir = path.parent().expect("a height file has a segment dir");
        if !dir.exists() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            sync_parent(dir)?;
        }
        let mut tmp = path.clone().into_os_string();
        tmp.push(TMP_SUFFIX);
        let tmp = PathBuf::from(tmp);
        {
            let mut out =
                File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            out.write_all(&bytes)
                .with_context(|| format!("writing {}", tmp.display()))?;
            out.sync_data()
                .with_context(|| format!("syncing {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} into place", tmp.display()))?;
        sync_parent(&path)
    }

    /// The whole file of `height` in `stream`, checksums verified; `None`
    /// when it does not exist.
    pub fn read(&self, stream: Stream, height: u32) -> Result<Option<HeightFile>> {
        let path = self.path(stream, height);
        let Some(mut input) = open(&path)? else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        input
            .read_to_end(&mut raw)
            .with_context(|| format!("reading {}", path.display()))?;
        ensure!(raw.len() >= HEADER_LEN, "{} is truncated", path.display());
        let header = parse_header(&raw[..HEADER_LEN], &path, stream, height)?;
        ensure!(
            raw.len() == HEADER_LEN + header.head_len + header.body_len,
            "{} is {} bytes, its header says {}",
            path.display(),
            raw.len(),
            HEADER_LEN + header.head_len + header.body_len
        );
        let head = &raw[HEADER_LEN..HEADER_LEN + header.head_len];
        let body = &raw[HEADER_LEN + header.head_len..];
        ensure!(
            crc32fast::hash(head) == header.head_crc && crc32fast::hash(body) == header.body_crc,
            "{} fails its checksum",
            path.display()
        );
        let decompressed;
        let body = if header.flags & COMPRESSED != 0 {
            decompressed = zstd::stream::decode_all(body)
                .with_context(|| format!("decompressing {}", path.display()))?;
            decompressed.as_slice()
        } else {
            body
        };
        let mut file = HeightFile::new(height, head.to_vec());
        decode_body(&mut file, body).with_context(|| format!("decoding {}", path.display()))?;
        Ok(Some(file))
    }

    /// The head of `height`'s file in `stream`, its checksum verified; `None`
    /// when the file does not exist.
    pub fn read_head(&self, stream: Stream, height: u32) -> Result<Option<Vec<u8>>> {
        let path = self.path(stream, height);
        let Some(mut input) = open(&path)? else {
            return Ok(None);
        };
        let mut raw = [0; HEADER_LEN];
        input
            .read_exact(&mut raw)
            .with_context(|| format!("reading {}", path.display()))?;
        let header = parse_header(&raw, &path, stream, height)?;
        let mut head = vec![0; header.head_len];
        input
            .read_exact(&mut head)
            .with_context(|| format!("reading {}", path.display()))?;
        ensure!(
            crc32fast::hash(&head) == header.head_crc,
            "{} fails its head checksum",
            path.display()
        );
        Ok(Some(head))
    }

    /// The first height of every segment dir of `stream`, ascending.
    pub fn segment_starts(&self, stream: Stream) -> Result<Vec<u32>> {
        numbered(&self.stream_dir(stream))
    }

    /// The heights with a file in the segment of `stream` starting at
    /// `start`, ascending.
    pub fn heights_in(&self, stream: Stream, start: u32) -> Result<Vec<u32>> {
        numbered(&self.segment_dir(stream, start))
    }

    /// The highest height with a file in `stream`.
    pub fn last_height(&self, stream: Stream) -> Result<Option<u32>> {
        for start in self.segment_starts(stream)?.into_iter().rev() {
            if let Some(height) = self.heights_in(stream, start)?.pop() {
                return Ok(Some(height));
            }
        }
        Ok(None)
    }

    /// Deletes the files of `stream` above `height`, every one when `None`,
    /// from the highest down. Segment dirs left empty go too.
    pub fn remove_above(&self, stream: Stream, height: Option<u32>) -> Result<()> {
        let from = match height {
            Some(u32::MAX) => return Ok(()),
            Some(height) => height + 1,
            None => 0,
        };
        for start in self.segment_starts(stream)?.into_iter().rev() {
            if start + (self.segment - 1) < from {
                break;
            }
            let dir = self.segment_dir(stream, start);
            for doomed in self
                .heights_in(stream, start)?
                .into_iter()
                .rev()
                .take_while(|doomed| *doomed >= from)
            {
                remove_if_exists(&self.path(stream, doomed))?;
                sync_parent(&self.path(stream, doomed))?;
            }
            if start >= from {
                remove_dir(&dir)?;
            }
        }
        Ok(())
    }

    /// Deletes the segment of `stream` starting at `start`.
    pub fn remove_segment_of(&self, stream: Stream, start: u32) -> Result<()> {
        remove_dir(&self.segment_dir(stream, start))
    }

    /// Bytes of every height file.
    pub fn disk_bytes(&self) -> Result<u64> {
        let mut bytes = 0;
        for stream in Stream::ALL {
            for start in self.segment_starts(stream)? {
                for entry in std::fs::read_dir(self.segment_dir(stream, start))? {
                    bytes += entry?.metadata()?.len();
                }
            }
        }
        Ok(bytes)
    }
}

/// Opens `path` for reading, `None` when it does not exist.
fn open(path: &Path) -> Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("opening {}", path.display())),
    }
}

/// Deletes the directory `dir` and its files, then syncs its parent.
fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => sync_parent(dir),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", dir.display())),
    }
}

/// The numbers that name the entries of `dir`, ascending; `.tmp` files and
/// other names are skipped. An absent dir has none.
fn numbered(dir: &Path) -> Result<Vec<u32>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };
    let mut numbers = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(number) = name.to_str().and_then(|name| name.parse().ok()) {
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    Ok(numbers)
}

fn decode_body(file: &mut HeightFile, body: &[u8]) -> Result<()> {
    let mut pos = 0;
    let take = |pos: &mut usize, len: usize| -> Result<&[u8]> {
        let end = pos.checked_add(len).filter(|end| *end <= body.len());
        let Some(end) = end else {
            bail!("body ends inside a row");
        };
        let bytes = &body[*pos..end];
        *pos = end;
        Ok(bytes)
    };
    let prefix = file.height.to_be_bytes();
    while pos < body.len() {
        let id = take(&mut pos, 1)?[0];
        let name = *TABLES
            .get(usize::from(id))
            .with_context(|| format!("unknown table id {id}"))?;
        let count = u32::from_le_bytes(take(&mut pos, 4)?.try_into()?);
        let mut rows = Rows::default();
        for _ in 0..count {
            let len = u32::from_le_bytes(take(&mut pos, 4)?.try_into()?) as usize;
            let mut key = prefix.to_vec();
            key.extend(take(&mut pos, len)?);
            let len = u32::from_le_bytes(take(&mut pos, 4)?.try_into()?) as usize;
            rows.0.insert(key, take(&mut pos, len)?.to_vec());
        }
        file.tables.insert(table_id(name), rows);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(height: u32) -> HeightFile {
        let mut file = HeightFile::new(height, vec![7; 80]);
        for idx in 0..50u32 {
            let mut key = height.to_be_bytes().to_vec();
            key.extend(idx.to_be_bytes());
            file.rows_mut(cf::TRANSFERS)
                .insert(&key, &[idx as u8; 100])
                .unwrap();
        }
        let mut key = height.to_be_bytes().to_vec();
        key.extend([1; 36]);
        file.rows_mut(cf::INFO).insert(&key, b"info").unwrap();
        file
    }

    #[test]
    fn files_round_trip_and_heads_read_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path(), 100, 0).unwrap();
        let file = sample(1234);
        store.write(Stream::Records, &file).unwrap();
        assert!(tmp.path().join("rec/000001200/000001234").exists());
        assert_eq!(store.read(Stream::Records, 1234).unwrap(), Some(file));
        assert_eq!(
            store.read_head(Stream::Records, 1234).unwrap(),
            Some(vec![7; 80])
        );
        assert_eq!(store.read(Stream::Raw, 1234).unwrap(), None);
        assert_eq!(store.read(Stream::Records, 1235).unwrap(), None);
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path(), 100, 0).unwrap();
        store.write(Stream::Records, &sample(5)).unwrap();
        let path = store.path(Stream::Records, 5);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(store.read(Stream::Records, 5).is_err());
    }

    #[test]
    fn remove_above_crosses_segments_and_drops_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path(), 100, 0).unwrap();
        for height in 95..=210 {
            store.write(Stream::Raw, &sample(height)).unwrap();
        }
        store.remove_above(Stream::Raw, Some(98)).unwrap();
        assert_eq!(store.segment_starts(Stream::Raw).unwrap(), [0]);
        assert_eq!(store.heights_in(Stream::Raw, 0).unwrap(), [95, 96, 97, 98]);
        store.remove_above(Stream::Raw, None).unwrap();
        assert_eq!(store.last_height(Stream::Raw).unwrap(), None);
    }

    #[test]
    fn tmp_files_of_interrupted_writes_are_deleted_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path(), 100, 0).unwrap();
        store.write(Stream::Raw, &sample(3)).unwrap();
        let torn = tmp.path().join("raw/000000000/000000004.tmp");
        std::fs::write(&torn, b"torn").unwrap();
        let store = Store::new(tmp.path(), 100, 0).unwrap();
        assert!(!torn.exists());
        assert_eq!(store.last_height(Stream::Raw).unwrap(), Some(3));
    }
}
