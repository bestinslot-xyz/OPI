//! Prune: deletes the segments no module will read again.
//!
//! A height's raw events serve only to finalize it, so once it is finalized
//! and deeper than the reorg window they can go. Its records serve the
//! modules that apply it and the journal's reorg check above the agreed
//! height, so they can go once every module has applied it and it is deeper
//! than the reorg window. A segment goes as a whole once every height in it
//! is below both bounds, so disk use follows from the window: it holds the
//! window's segments, the one being written and the one the window starts
//! in. The tracked set drops the inscriptions transferred below the raw
//! bound, which no rollback reaches.

use {
    super::{Journal, META_PRUNED_BELOW},
    anyhow::{Result, ensure},
};

/// Log rows one tracked-set prune transaction deletes at most, which keeps
/// it to well under a second.
pub const TRACKED_STEP_ROWS: u64 = 100_000;

/// What one [`Journal::prune`] call deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    /// Segment files.
    pub segments: u64,
    /// Rows of inscriptions whose first transfer is below the window.
    pub tracked: u64,
}

impl Pruned {
    pub fn is_empty(&self) -> bool {
        *self == Pruned::default()
    }
}

impl Journal {
    /// Deletes every segment whose heights are all below `raw_below` and
    /// `records_below`, below the segment of the final tip, then the tracked
    /// rows of inscriptions transferred below `raw_below`. Both bounds must
    /// be at most one above the final tip.
    ///
    /// The pruned floor rises before the files go, so a reader never finds a
    /// height above the floor missing. A reader holding a segment keeps
    /// reading it, and the file system frees its space once the reader lets
    /// go.
    pub fn prune(&self, raw_below: u32, records_below: u32) -> Result<Pruned> {
        let Some(tip) = self.final_tip()? else {
            return Ok(Pruned::default());
        };
        ensure!(
            raw_below <= tip + 1 && records_below <= tip + 1,
            "pruning below {raw_below} and {records_below}, above the final tip {tip}"
        );
        let below = raw_below.min(records_below).min(self.segment_start(tip));
        let doomed = self
            .segment_starts()
            .into_iter()
            .take_while(|start| start + self.segment_len() <= below)
            .collect::<Vec<_>>();
        let mut pruned = Pruned::default();
        if let Some(last) = doomed.last() {
            let floor = last + self.segment_len();
            if floor > self.pruned_below()? {
                self.set_meta_u32(META_PRUNED_BELOW, floor)?;
            }
            let mut segments = self.inner.segments.lock().unwrap();
            for start in doomed {
                segments.delete(start)?;
                pruned.segments += 1;
            }
        }
        loop {
            let (dropped, done) = self.prune_tracked(raw_below, TRACKED_STEP_ROWS)?;
            pruned.tracked += dropped;
            if done {
                return Ok(pruned);
            }
        }
    }

    /// Total bytes of the journal's files.
    pub fn disk_bytes(&self) -> Result<u64> {
        let mut bytes = 0;
        for entry in std::fs::read_dir(self.dir())? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                bytes += entry.metadata()?.len();
            }
        }
        Ok(bytes)
    }
}
