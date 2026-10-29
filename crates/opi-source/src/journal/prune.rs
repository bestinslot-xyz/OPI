//! Prune: deletes journal rows no module will read again.
//!
//! A height's raw events serve only to finalize it, so once it is finalized
//! and deeper than the reorg window they can go. A height's transfer records
//! and per-block rune rows serve the modules that apply it and the journal's
//! reorg check above the agreed height, so they can go once every module has
//! applied it and it is deeper than the reorg window. An info row serves the
//! inscription's creation and its first transfer, the only transfer the
//! journal records, so it goes with the record of that first transfer. The
//! info rows of inscriptions never transferred stay, as do `first_transfer`,
//! the headers and the current rune state.

use {
    super::{
        Journal, META_PRUNED_BELOW, cf, decode_transfer, info_key, meta_u32_in, table, transfer_key,
    },
    anyhow::{Result, ensure},
    redb::{ReadableDatabase, ReadableTable},
};

/// Most heights one prune transaction covers, so a first prune of a long
/// history commits in steps.
pub const PRUNE_BATCH: u32 = 1000;

/// What one [`Journal::prune`] call deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    pub raw_events: u64,
    pub transfers: u64,
    pub infos: u64,
    pub rune_rows: u64,
}

impl Pruned {
    pub fn is_empty(&self) -> bool {
        *self == Pruned::default()
    }
}

impl Journal {
    /// Lowest height whose transfer records and per-block rune rows the
    /// journal still holds; 0 when nothing was pruned.
    pub fn pruned_below(&self) -> Result<u32> {
        Ok(meta_u32_in(&self.db.begin_read()?, META_PRUNED_BELOW, "pruned_below")?.unwrap_or(0))
    }

    /// Deletes the raw events of heights below `raw_below`, and the transfer
    /// records, per-block rune rows and consumed info rows of heights below
    /// `records_below`, then raises the pruned floor to `records_below`.
    /// Both bounds must be at most one above the final tip. A bound at or
    /// below what was already pruned deletes nothing for it. Each step of
    /// [`PRUNE_BATCH`] heights commits on its own.
    pub fn prune(&self, raw_below: u32, records_below: u32) -> Result<Pruned> {
        let next = self.final_tip()?.map_or(0, |tip| tip + 1);
        ensure!(
            raw_below <= next && records_below <= next,
            "pruning below {raw_below} and {records_below}, above the final tip {next}"
        );
        let mut pruned = Pruned::default();
        let raw_from = self.raw_floor()?.unwrap_or(raw_below);
        for from in (raw_from..raw_below).step_by(PRUNE_BATCH as usize) {
            pruned.raw_events += self.prune_raw(from, raw_below.min(from + PRUNE_BATCH))?;
        }
        let records_from = self.pruned_below()?;
        for from in (records_from..records_below).step_by(PRUNE_BATCH as usize) {
            let to = records_below.min(from + PRUNE_BATCH);
            let step = self.prune_records(from, to)?;
            pruned.transfers += step.transfers;
            pruned.infos += step.infos;
            pruned.rune_rows += step.rune_rows;
        }
        Ok(pruned)
    }

    /// Lowest height with a raw event or height tag, if any.
    pub fn raw_floor(&self) -> Result<Option<u32>> {
        let tx = self.db.begin_read()?;
        let mut lowest = None::<u32>;
        for name in [cf::RAW_EVENTS, cf::RAW_HEIGHTS] {
            if let Some((key, _)) = tx.open_table(table(name))?.first()? {
                let height = u32::from_be_bytes(key.value()[..4].try_into()?);
                lowest = Some(lowest.map_or(height, |lowest| lowest.min(height)));
            }
        }
        Ok(lowest)
    }

    /// Deletes the raw events and height tags of `from..to`.
    fn prune_raw(&self, from: u32, to: u32) -> Result<u64> {
        let tx = self.begin_write()?;
        let mut deleted = 0;
        {
            let mut events = tx.open_table(table(cf::RAW_EVENTS))?;
            let (first, end) = (transfer_key(from, 0), transfer_key(to, 0));
            events.retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| {
                deleted += 1;
                false
            })?;
            let mut heights = tx.open_table(table(cf::RAW_HEIGHTS))?;
            let (first, end) = (from.to_be_bytes(), to.to_be_bytes());
            heights.retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// Deletes the transfer records and per-block rune rows of `from..to`,
    /// and the info rows of the inscriptions whose first transfer they
    /// record, and sets the pruned floor to `to`, in one transaction.
    fn prune_records(&self, from: u32, to: u32) -> Result<Pruned> {
        let tx = self.begin_write()?;
        let mut pruned = Pruned::default();
        {
            let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
            let mut infos = tx.open_table(table(cf::INFO))?;
            let (first, end) = (transfer_key(from, 0), transfer_key(to, 0));
            let mut consumed = Vec::new();
            for row in transfers.range::<&[u8]>(first.as_slice()..end.as_slice())? {
                let (key, value) = row?;
                let record = decode_transfer(key.value(), value.value())?;
                if record.old_satpoint.is_some() {
                    consumed.push(info_key(&record.inscription_id)?);
                }
                pruned.transfers += 1;
            }
            for id in consumed {
                if infos.remove(id.as_slice())?.is_some() {
                    pruned.infos += 1;
                }
            }
            transfers.retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)?;

            if self.runes {
                for name in [cf::RUNE_EVENTS, cf::RUNE_ETCHINGS, cf::RUNE_COUNTER_LOG] {
                    let mut rows = tx.open_table(table(name))?;
                    let (first, end) = (from.to_be_bytes(), to.to_be_bytes());
                    rows.retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| {
                        pruned.rune_rows += 1;
                        false
                    })?;
                }
            }

            tx.open_table(table(cf::META))?
                .insert(META_PRUNED_BELOW, to.to_be_bytes().as_slice())?;
        }
        tx.commit()?;
        Ok(pruned)
    }

    /// Rewrites the journal file at `path` without its free pages, so the
    /// space pruning freed returns to the file system. Needs the journal
    /// closed: redb holds an exclusive lock on an open file.
    pub fn compact(path: &std::path::Path, options: &super::JournalOptions) -> Result<bool> {
        let mut db = options.builder().open(path)?;
        Ok(db.compact()?)
    }
}
