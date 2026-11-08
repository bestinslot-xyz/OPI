//! Reorg check: rolls the finalized journal back to the highest block it
//! shares with ord's index.

use {
    super::{Journal, Stream},
    anyhow::{Context, Result},
    bitcoin::{BlockHash, block::Header, consensus},
    ord::Index,
};

/// Compares the finalized header hashes from the final tip down with ord's
/// block hashes. On a mismatch it rolls the journal back to the highest
/// agreeing height and returns it: the record files above that height go,
/// from the highest down, then the tracked set drops what heights above it
/// changed. A crash part way leaves a lower final tip whose records are
/// intact, and the next check finishes the rollback.
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

/// Rolls the finalized journal back to `agreed`: deletes the record files
/// above it from the highest down, across segment boundaries, then undoes
/// the tracked set's changes above it.
pub fn rollback_to(journal: &Journal, agreed: u32) -> Result<()> {
    journal
        .store()
        .remove_above(Stream::Records, Some(agreed))?;
    journal.load_final_tip()?;
    journal.tracked_undo_above(Some(agreed))
}

/// The hash of the finalized header at `height`, `None` when the journal
/// holds none there.
fn header_hash(journal: &Journal, height: u32) -> Result<Option<BlockHash>> {
    let Some(head) = journal.store().read_head(Stream::Records, height)? else {
        return Ok(None);
    };
    let header: Header = consensus::deserialize(&head)?;
    Ok(Some(header.block_hash()))
}
