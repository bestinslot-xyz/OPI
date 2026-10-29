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

/// Row budget of one prune transaction. A raw event transaction deletes at
/// most this many events. A records transaction keeps whole heights, since
/// the pruned floor is a height: it ends at the first height boundary past
/// this many rows of any table it deletes from, so it deletes at most this
/// many rows plus one height's rows per table, and one height always fits.
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
    /// journal still holds; 0 when nothing was pruned.
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
    /// events, deleting at most `max_rows` of them, and one for the records,
    /// ending at the first height boundary past `max_rows` rows of a table,
    /// each within [`PRUNE_BATCH`] heights. Both bounds must be at most one
    /// above the final tip. A bound at or below what was already pruned
    /// deletes nothing for it.
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
        let records_from = self.pruned_below()?;
        let records_done = if records_from < records_below {
            let to = records_below.min(records_from.saturating_add(PRUNE_BATCH));
            let (pruned, end) = self.prune_records(records_from, to, max_rows)?;
            step.pruned.add(pruned);
            end >= records_below
        } else {
            true
        };
        step.done = raw_done && records_done;
        Ok(step)
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
    /// the height tags of the heights it empties, in one transaction. A
    /// height whose events it deletes only in part keeps the rest and its
    /// tag, so the raw floor stays at that height and the next step resumes
    /// there; only finalize reads raw events, and only above the final tip.
    /// Returns the events deleted and the height below which nothing is
    /// left, `to` when it deleted all of `from..to`.
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
            let (first, end) = (from.to_be_bytes(), end.to_be_bytes());
            heights.retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)?;
            u32::from_be_bytes(end)
        };
        tx.commit()?;
        Ok((deleted, end))
    }

    /// Deletes the transfer records and per-block rune rows of `from..end`,
    /// and the info rows of the inscriptions whose first transfer they
    /// record, and sets the pruned floor to `end`, in one transaction. `end`
    /// is at most `to` and ends the range at the first height boundary past
    /// `max_rows` rows of any of those tables. Returns what was deleted and
    /// `end`.
    fn prune_records(&self, from: u32, to: u32, max_rows: u64) -> Result<(Pruned, u32)> {
        let tx = self.begin_write()?;
        let mut pruned = Pruned::default();
        let to = {
            let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
            let mut infos = tx.open_table(table(cf::INFO))?;
            let mut to = rows_end(&transfers, from, to, max_rows)?;
            if self.runes {
                for name in [cf::RUNE_EVENTS, cf::RUNE_ETCHINGS, cf::RUNE_COUNTER_LOG] {
                    to = rows_end(&tx.open_table(table(name))?, from, to, max_rows)?;
                }
            }
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
            to
        };
        tx.commit()?;
        Ok((pruned, to))
    }
}

/// The end of a prune range over `rows`, a table keyed by height (`u32` BE)
/// first: `to`, or the first height above `from` at which the rows of
/// `from..` reach `max_rows`, whichever is lower. Every height below it is
/// whole, and the first height is always inside.
fn rows_end(
    rows: &impl ReadableTable<&'static [u8], &'static [u8]>,
    from: u32,
    to: u32,
    max_rows: u64,
) -> Result<u32> {
    let (first, end) = (from.to_be_bytes(), to.to_be_bytes());
    let mut current = None;
    for (counted, row) in (0u64..).zip(rows.range::<&[u8]>(first.as_slice()..end.as_slice())?) {
        let (key, _) = row?;
        let height = u32::from_be_bytes(key.value()[..4].try_into()?);
        if current != Some(height) {
            if counted >= max_rows {
                return Ok(height);
            }
            current = Some(height);
        }
    }
    Ok(to)
}

impl Journal {
    /// Rewrites the journal file at `path` without its free pages, so the
    /// space pruning freed returns to the file system. Needs the journal
    /// closed: redb holds an exclusive lock on an open file.
    pub fn compact(path: &std::path::Path) -> Result<bool> {
        let mut db = redb::Database::open(path)?;
        Ok(db.compact()?)
    }
}
