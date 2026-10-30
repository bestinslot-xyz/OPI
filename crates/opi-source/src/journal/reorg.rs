//! Reorg check: rolls the finalized journal back to the highest block it
//! shares with ord's index.

use {
    super::{
        Journal, begin_write, cf, delete_records_from, segment_tip_in, set_segment_tip, table,
    },
    anyhow::{Context, Result},
    bitcoin::{BlockHash, block::Header, consensus},
    ord::Index,
    redb::ReadableDatabase,
};

/// Compares the finalized header hashes from the final tip down with ord's
/// block hashes. On a mismatch it rolls the journal back to the highest
/// agreeing height and returns it: each segment above that height, from the
/// highest down, deletes its records above it and lowers its final tip in
/// one transaction of its own, then the tracked set drops what heights above
/// it changed. A crash part way leaves a final tip whose segment is intact,
/// and the next check finishes the rollback.
///
/// Returns `None` when the tip agrees or nothing is finalized, and fails when
/// no finalized height the journal still holds agrees.
pub fn reorg_check(journal: &Journal, index: &Index) -> Result<Option<u32>> {
    let Some(tip) = journal.final_tip()? else {
        return Ok(None);
    };

    let mut height = tip;
    let agreed = loop {
        let stored = header_hash(journal, height)?;
        if stored.is_some() && stored == index.block_hash(Some(height))? {
            break height;
        }
        height = height.checked_sub(1).context(
            "the journal shares no finalized block with ord's index; \
             delete the journal or restore ord's data dir",
        )?;
    };
    if agreed == tip {
        return Ok(None);
    }
    rollback_to(journal, agreed)?;
    Ok(Some(agreed))
}

/// Rolls the finalized journal back to `agreed`, from the highest segment
/// down, each segment in a transaction of its own.
pub fn rollback_to(journal: &Journal, agreed: u32) -> Result<()> {
    for start in journal.segment_starts().into_iter().rev() {
        let last = start + (journal.segment_len() - 1);
        if last <= agreed {
            break;
        }
        let Some(segment) = journal.segment(start)? else {
            continue;
        };
        let tip = segment_tip_in(&segment.begin_read()?)?;
        let tx = begin_write(&segment)?;
        delete_records_from(&tx, (agreed + 1).max(start))?;
        set_segment_tip(
            &tx,
            tip.map(|tip| tip.min(agreed)).filter(|tip| *tip >= start),
        )?;
        tx.commit()?;
        journal.load_final_tip()?;
    }
    journal.tracked_undo_above(Some(agreed))
}

/// The hash of the finalized header at `height`, `None` when the journal
/// holds none there.
fn header_hash(journal: &Journal, height: u32) -> Result<Option<BlockHash>> {
    let Some(segment) = journal.segment(height)? else {
        return Ok(None);
    };
    let tx = segment.begin_read()?;
    let headers = tx.open_table(table(cf::HEADERS))?;
    let Some(header) = headers.get(height.to_be_bytes().as_slice())? else {
        return Ok(None);
    };
    let header: Header = consensus::deserialize(header.value())?;
    Ok(Some(header.block_hash()))
}
