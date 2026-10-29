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

/// Most heights one prune transaction covers.
pub const PRUNE_BATCH: u32 = 1000;

/// Row budget of one prune transaction: a raw event transaction deletes at
/// most this many events, and a records transaction at most this many
/// transfer records, consumed info rows and per-block rune rows together.
/// Either may stop inside a height; the height then fails closed (see
/// [`Journal::prune_step`]).
/// The budget keeps each write transaction to about a second, well within
/// ord's 15 s bitcoind read timeout, since ord's commit barrier waits on the
/// stager's journal writes. It also bounds the pages a transaction holds
/// before its commit frees them, and so the file growth during a prune.
pub const PRUNE_STEP_ROWS: u64 = 10_000;

/// What one [`Journal::prune`] or [`Journal::prune_step`] call deleted.
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

    pub fn add(&mut self, other: Pruned) {
        self.raw_events += other.raw_events;
        self.transfers += other.transfers;
        self.infos += other.infos;
        self.rune_rows += other.rune_rows;
    }
}

/// The outcome of one [`Journal::prune_step`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneStep {
    pub pruned: Pruned,
    /// Whether the journal holds nothing more to prune below the bounds.
    pub done: bool,
}

impl Journal {
    /// Lowest height whose transfer records and per-block rune rows the
    /// journal serves; 0 when nothing was pruned. Rows of lower heights that
    /// a stopped step left behind stay unread until the next step deletes
    /// them.
    pub fn pruned_below(&self) -> Result<u32> {
        Ok(meta_u32_in(&self.db.begin_read()?, META_PRUNED_BELOW, "pruned_below")?.unwrap_or(0))
    }

    /// Deletes the raw events of heights below `raw_below`, and the transfer
    /// records, per-block rune rows and consumed info rows of heights below
    /// `records_below`, then raises the pruned floor to `records_below`, in
    /// steps of [`Journal::prune_step`] that each commit on their own.
    pub fn prune(&self, raw_below: u32, records_below: u32) -> Result<Pruned> {
        let mut pruned = Pruned::default();
        loop {
            let step = self.prune_step(raw_below, records_below, PRUNE_STEP_ROWS)?;
            pruned.add(step.pruned);
            if step.done {
                return Ok(pruned);
            }
        }
    }

    /// One step of [`Journal::prune`]: at most one transaction for the raw
    /// events and one for the records, each deleting at most `max_rows` rows
    /// within [`PRUNE_BATCH`] heights. Both bounds must be at most one above
    /// the final tip. A bound at or below what was already pruned deletes
    /// nothing for it.
    ///
    /// A transaction that stops inside a height makes that height fail
    /// closed in the same commit: a raw height loses its tag, so nothing
    /// reads the rest of a cut height as that block's log (finalize works
    /// only above the final tip), and a records height goes below the pruned
    /// floor, so the reader refuses it. The next step resumes from the
    /// lowest row left.
    pub fn prune_step(
        &self,
        raw_below: u32,
        records_below: u32,
        max_rows: u64,
    ) -> Result<PruneStep> {
        ensure!(max_rows > 0, "a prune step needs a positive row budget");
        let next = self.final_tip()?.map_or(0, |tip| tip + 1);
        ensure!(
            raw_below <= next && records_below <= next,
            "pruning below {raw_below} and {records_below}, above the final tip {next}"
        );
        let mut step = PruneStep::default();
        let raw_from = self.raw_floor()?.unwrap_or(raw_below);
        let raw_done = if raw_from < raw_below {
            let to = raw_below.min(raw_from.saturating_add(PRUNE_BATCH));
            let (deleted, end) = self.prune_raw(raw_from, to, max_rows)?;
            step.pruned.raw_events = deleted;
            end >= raw_below
        } else {
            true
        };
        let records_from = self.records_floor()?;
        let records_done = if records_from < records_below {
            let to = records_below.min(records_from.saturating_add(PRUNE_BATCH));
            let (pruned, whole) = self.prune_records(to, max_rows)?;
            step.pruned.add(pruned);
            whole && to >= records_below
        } else {
            true
        };
        step.done = raw_done && records_done;
        Ok(step)
    }

    /// The tables holding a height's records, all keyed by height (`u32` BE)
    /// first.
    fn record_tables(&self) -> &'static [&'static str] {
        if self.runes {
            &[
                cf::TRANSFERS,
                cf::RUNE_EVENTS,
                cf::RUNE_ETCHINGS,
                cf::RUNE_COUNTER_LOG,
            ]
        } else {
            &[cf::TRANSFERS]
        }
    }

    /// Lowest height a records step starts from: the pruned floor, or the
    /// lowest record row below it that a stopped step left.
    pub fn records_floor(&self) -> Result<u32> {
        let floor = self.pruned_below()?;
        let tx = self.db.begin_read()?;
        let mut lowest = floor;
        for name in self.record_tables() {
            if let Some((key, _)) = tx.open_table(table(name))?.first()? {
                lowest = lowest.min(u32::from_be_bytes(key.value()[..4].try_into()?));
            }
        }
        Ok(lowest)
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

    /// Deletes at most `max_rows` raw events of `from..to`, lowest first, and
    /// the height tags of `from` up to the height it stops in, in one
    /// transaction. A height whose events it deletes only in part keeps the
    /// rest without its tag, so nothing reads the rest of a cut height as
    /// that block's log (finalize works only above the final tip), and the
    /// raw floor stays at it for the next step. Returns the events deleted and the height
    /// below which nothing is left, `to` when it deleted all of `from..to`.
    fn prune_raw(&self, from: u32, to: u32, max_rows: u64) -> Result<(u64, u32)> {
        let tx = self.begin_write()?;
        let mut deleted = 0;
        let end = {
            let mut events = tx.open_table(table(cf::RAW_EVENTS))?;
            let first = transfer_key(from, 0);
            let cut = match events
                .range::<&[u8]>(first.as_slice()..transfer_key(to, 0).as_slice())?
                .nth(usize::try_from(max_rows).unwrap_or(usize::MAX))
                .transpose()?
            {
                Some((key, _)) => <[u8; 8]>::try_from(key.value())?,
                None => transfer_key(to, 0),
            };
            events.retain_in::<&[u8], _>(first.as_slice()..cut.as_slice(), |_, _| {
                deleted += 1;
                false
            })?;
            let end = u32::from_be_bytes(cut[..4].try_into()?);
            let mut heights = tx.open_table(table(cf::RAW_HEIGHTS))?;
            let first = from.to_be_bytes();
            if end < to {
                let last = end.to_be_bytes();
                heights.retain_in::<&[u8], _>(first.as_slice()..=last.as_slice(), |_, _| false)?;
            } else {
                let last = to.to_be_bytes();
                heights.retain_in::<&[u8], _>(first.as_slice()..last.as_slice(), |_, _| false)?;
            }
            end
        };
        tx.commit()?;
        Ok((deleted, end))
    }

    /// Deletes, lowest first, the record rows of every height below `to`:
    /// transfer records with the info rows of the inscriptions whose first
    /// transfer they record, then the per-block rune rows, at most
    /// `max_rows` rows in all, in one transaction. It raises the pruned
    /// floor in the same commit: to `to` when no record row below `to` is
    /// left, else to one above the highest height it deleted from, so a
    /// height it left in part is already below the floor. Returns what was
    /// deleted and whether no record row below `to` is left.
    fn prune_records(&self, to: u32, max_rows: u64) -> Result<(Pruned, bool)> {
        let tx = self.begin_write()?;
        let mut pruned = Pruned::default();
        let mut budget = max_rows;
        let mut highest = None::<u32>;
        let mut whole = true;
        let end = to.to_be_bytes();
        for &name in self.record_tables() {
            if budget == 0 {
                whole = false;
                break;
            }
            let mut rows = tx.open_table(table(name))?;
            // The key of the first row this transaction keeps, if any.
            let mut cut = None::<Vec<u8>>;
            let mut consumed = Vec::new();
            let mut taken = 0u64;
            for row in rows.range::<&[u8]>(..end.as_slice())? {
                let (key, value) = row?;
                // A first transfer consumes its inscription's info row.
                let info = if name == cf::TRANSFERS {
                    let record = decode_transfer(key.value(), value.value())?;
                    record
                        .old_satpoint
                        .map(|_| info_key(&record.inscription_id))
                        .transpose()?
                } else {
                    None
                };
                let cost = 1 + u64::from(info.is_some());
                // The first row always fits, so every step makes progress.
                if taken > 0 && taken + cost > budget {
                    cut = Some(key.value().to_vec());
                    break;
                }
                consumed.extend(info);
                taken += cost;
                let height = u32::from_be_bytes(key.value()[..4].try_into()?);
                highest = Some(highest.map_or(height, |highest| highest.max(height)));
            }
            budget = budget.saturating_sub(taken);
            let mut deleted = 0;
            match &cut {
                Some(cut) => rows.retain_in::<&[u8], _>(..cut.as_slice(), |_, _| {
                    deleted += 1;
                    false
                })?,
                None => rows.retain_in::<&[u8], _>(..end.as_slice(), |_, _| {
                    deleted += 1;
                    false
                })?,
            }
            drop(rows);
            if name == cf::TRANSFERS {
                pruned.transfers += deleted;
                let mut infos = tx.open_table(table(cf::INFO))?;
                for id in consumed {
                    if infos.remove(id.as_slice())?.is_some() {
                        pruned.infos += 1;
                    }
                }
            } else {
                pruned.rune_rows += deleted;
            }
            if cut.is_some() {
                whole = false;
                break;
            }
        }
        let floor = if whole {
            to
        } else {
            highest.map_or(0, |highest| highest + 1)
        };
        {
            let mut meta = tx.open_table(table(cf::META))?;
            let current = meta_u32_in_table(&meta)?;
            if floor > current {
                meta.insert(META_PRUNED_BELOW, floor.to_be_bytes().as_slice())?;
            }
        }
        tx.commit()?;
        Ok((pruned, whole))
    }
}

/// The pruned floor as `meta` holds it; 0 when nothing was pruned.
fn meta_u32_in_table(meta: &impl ReadableTable<&'static [u8], &'static [u8]>) -> Result<u32> {
    let Some(raw) = meta.get(META_PRUNED_BELOW)? else {
        return Ok(0);
    };
    let bytes: [u8; 4] = raw
        .value()
        .try_into()
        .map_err(|_| anyhow::anyhow!("pruned_below is {} bytes, expected 4", raw.value().len()))?;
    Ok(u32::from_be_bytes(bytes))
}

impl Journal {
    /// Rewrites the journal file at `path` without its free pages, so the
    /// space pruning freed returns to the file system. Needs the journal
    /// closed: redb holds an exclusive lock on an open file.
    pub fn compact(path: &std::path::Path, options: &super::JournalOptions) -> Result<bool> {
        let mut db = options.builder().open(path)?;
        Ok(db.compact()?)
    }
}
