//! Reorg check: rolls the finalized journal back to the highest block it
//! shares with ord's index.

use {
    super::{Journal, cf, decode_transfer, info_key, set_final_tip, table, transfer_key},
    anyhow::{Context, Result},
    bitcoin::{BlockHash, block::Header, consensus},
    ord::Index,
    redb::ReadableTable,
};

/// Compares the finalized header hashes from `final_tip` down with ord's
/// block hashes. On a mismatch it deletes every transfer record and header
/// above the highest agreeing height, the info rows of inscriptions created
/// and the first-transfer entries of transfers recorded above it, sets
/// `final_tip` to that height and returns it. Returns `None` when the tip
/// agrees or nothing is finalized, and fails when no finalized height agrees.
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

    let tx = journal.begin_write()?;
    {
        let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
        let mut infos = tx.open_table(table(cf::INFO))?;
        let mut headers = tx.open_table(table(cf::HEADERS))?;
        let mut first_transfers = tx.open_table(table(cf::FIRST_TRANSFER))?;

        // Every first-transfer entry above `agreed` belongs to a transfer
        // record above it, so the records name every entry to delete.
        let first = transfer_key(agreed + 1, 0);
        for row in transfers.range::<&[u8]>(first.as_slice()..)? {
            let (key, value) = row?;
            let record = decode_transfer(key.value(), value.value())?;
            let id = info_key(&record.inscription_id)?;
            if record.old_satpoint.is_none() {
                infos.remove(id.as_slice())?;
            } else {
                let above = first_transfers
                    .get(id.as_slice())?
                    .is_some_and(|height| height.value() > agreed.to_be_bytes().as_slice());
                if above {
                    first_transfers.remove(id.as_slice())?;
                }
            }
        }
        transfers.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;

        let first = (agreed + 1).to_be_bytes();
        headers.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    }
    set_final_tip(&tx, Some(agreed))?;
    tx.commit()?;
    Ok(Some(agreed))
}

fn header_hash(journal: &Journal, height: u32) -> Result<Option<BlockHash>> {
    let tx = journal.db().begin_read()?;
    let headers = tx.open_table(table(cf::HEADERS))?;
    let Some(header) = headers.get(height.to_be_bytes().as_slice())? else {
        return Ok(None);
    };
    let header: Header = consensus::deserialize(header.value())?;
    Ok(Some(header.block_hash()))
}
