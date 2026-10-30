//! The segment files of a journal and the handles open on them.
//!
//! The registry keeps the [`OPEN_SEGMENTS`] most recently used segments
//! open; the others close once nothing holds them and open on their next
//! use. A handle is an `Arc`, so a reader or writer holding one keeps using
//! it after the registry lets it go, and every caller gets that same handle
//! while it lives, since redb opens a file only once. A prune that deletes a
//! segment's file leaves the file's space to the file system once the last
//! holder drops its handle.

use {
    super::{
        JournalOptions, SEGMENT_TABLES, begin_write, create_atomically, create_db, init_path,
        is_in_use, remove_if_exists, sync_parent, table,
    },
    anyhow::{Context, Result},
    redb::Database,
    std::{
        collections::{BTreeSet, HashMap, VecDeque},
        path::{Path, PathBuf},
        sync::{Arc, Weak},
    },
};

/// Segments open at once: the one being written, the one before it, which
/// finalize and the modules still read after a boundary, and one more for a
/// reader further back.
pub const OPEN_SEGMENTS: usize = 3;

/// Longest a segment's open waits for the close of its previous handle.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The file name prefix of a segment.
pub const SEGMENT_PREFIX: &str = "seg-";

/// The file name of the segment starting at `start`.
pub fn segment_name(start: u32) -> String {
    format!("{SEGMENT_PREFIX}{start:09}.redb")
}

/// The first height of the segment file called `name`.
fn segment_start(name: &str) -> Option<u32> {
    name.strip_prefix(SEGMENT_PREFIX)?
        .strip_suffix(".redb")?
        .parse()
        .ok()
}

pub(super) struct Segments {
    dir: PathBuf,
    cache_bytes: usize,
    len: u32,
    /// The first height of every segment file.
    files: BTreeSet<u32>,
    /// The segments the registry keeps open, most recently used first.
    open: VecDeque<(u32, Arc<Database>)>,
    /// Every handle handed out, alive while anything holds it.
    handles: HashMap<u32, Weak<Database>>,
}

impl Segments {
    /// The segments in `dir`. Deletes leftovers of a segment creation that
    /// did not finish.
    pub(super) fn scan(dir: &Path, options: &JournalOptions) -> Result<Segments> {
        let mut files = BTreeSet::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(SEGMENT_PREFIX) && name.ends_with(".init") {
                remove_if_exists(&entry.path())?;
            } else if let Some(start) = segment_start(name) {
                anyhow::ensure!(
                    start % options.segment == 0,
                    "{} does not start a segment of {} heights",
                    entry.path().display(),
                    options.segment
                );
                files.insert(start);
            }
        }
        Ok(Segments {
            dir: dir.to_path_buf(),
            cache_bytes: options.segment_cache(),
            len: options.segment,
            files,
            open: VecDeque::new(),
            handles: HashMap::new(),
        })
    }

    pub(super) fn starts(&self) -> Vec<u32> {
        self.files.iter().copied().collect()
    }

    fn path(&self, start: u32) -> PathBuf {
        self.dir.join(segment_name(start))
    }

    /// The segment starting at `start`, `None` when it has no file.
    pub(super) fn get(&mut self, start: u32) -> Result<Option<Arc<Database>>> {
        if let Some(position) = self.open.iter().position(|(open, _)| *open == start) {
            let entry = self.open.remove(position).unwrap();
            let db = entry.1.clone();
            self.open.push_front(entry);
            return Ok(Some(db));
        }
        if !self.files.contains(&start) {
            return Ok(None);
        }
        let db = match self.handles.get(&start).and_then(Weak::upgrade) {
            Some(db) => db,
            None => {
                let db = Arc::new(self.open_file(start)?);
                self.handles.retain(|_, handle| handle.strong_count() > 0);
                self.handles.insert(start, Arc::downgrade(&db));
                db
            }
        };
        self.open.push_front((start, db.clone()));
        self.open.truncate(OPEN_SEGMENTS);
        Ok(Some(db))
    }

    /// Opens the file of the segment starting at `start`. The last holder of
    /// its previous handle may still be closing it, and redb keeps the file
    /// locked until the close ends, so a locked file is retried for up to
    /// [`CLOSE_WAIT`].
    fn open_file(&self, start: u32) -> Result<Database> {
        let path = self.path(start);
        let deadline = std::time::Instant::now() + CLOSE_WAIT;
        loop {
            match create_db(&path, self.cache_bytes) {
                Err(err) if is_in_use(&err) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    /// The segment starting at `start`, its file built with every table
    /// when absent.
    pub(super) fn get_or_create(&mut self, start: u32) -> Result<Arc<Database>> {
        debug_assert_eq!(start % self.len, 0);
        if !self.files.contains(&start) {
            create_atomically(&self.path(start), 1 << 20, |db| {
                let tx = begin_write(db)?;
                for name in SEGMENT_TABLES {
                    tx.open_table(table(name))?;
                }
                tx.commit()?;
                Ok(())
            })?;
            self.files.insert(start);
        }
        Ok(self.get(start)?.expect("the segment file exists"))
    }

    /// Deletes the segment starting at `start`. Holders of its handle keep
    /// reading the deleted file until they drop it.
    pub(super) fn delete(&mut self, start: u32) -> Result<()> {
        self.open.retain(|(open, _)| *open != start);
        self.handles.remove(&start);
        self.files.remove(&start);
        let path = self.path(start);
        remove_if_exists(&path)?;
        remove_if_exists(&init_path(&path))?;
        sync_parent(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_by_height_and_parse_back() {
        assert_eq!(segment_name(824_000), "seg-000824000.redb");
        assert!(segment_name(9_000) < segment_name(10_000));
        assert_eq!(
            segment_start(&segment_name(4_294_967_000)),
            Some(4_294_967_000)
        );
        assert_eq!(segment_start("meta.redb"), None);
    }
}
