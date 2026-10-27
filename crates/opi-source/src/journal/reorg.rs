//! Reorg check: rolls the finalized journal back to the highest block it
//! shares with ord's index.

use {
    super::{
        Journal, OUTPOINT_LEN, RuneModelError, cf, decode_rune_counter_log, decode_rune_event,
        decode_transfer, encode_rune_balance_row, encode_rune_counter, info_key, rune_event_key,
        rune_outpoint_key, set_final_tip, table, transfer_key,
    },
    anyhow::{Context, Result, bail},
    bitcoin::{BlockHash, block::Header, consensus},
    opi_core::{RuneEvent, RuneEventKind},
    ord::Index,
    redb::{ReadableDatabase, ReadableTable, WriteTransaction},
    std::collections::BTreeMap,
};

/// Compares the finalized header hashes from `final_tip` down with ord's
/// block hashes. On a mismatch it deletes every transfer record and header
/// above the highest agreeing height, the info rows of inscriptions created
/// and the first-transfer entries of transfers recorded above it, rolls the
/// rune tables of a journal that keeps them back to that height, sets
/// `final_tip` to that height and returns it, all in one write transaction.
/// Returns `None` when the tip agrees or nothing is finalized, and fails when
/// no finalized height agrees.
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
    if journal.runes() {
        undo_runes(&tx, agreed)?;
    }
    set_final_tip(&tx, Some(agreed))?;
    tx.commit()?;
    Ok(Some(agreed))
}

/// Rolls the rune tables back to their state after height `agreed`. Heights
/// above it are undone from the top down, and the transactions of a height
/// in reverse block order: each drops the outpoints it created from
/// `rune_outpoint_balances` and puts back the outpoints it spent, rebuilt
/// from its input events. An outpoint to drop that is absent, or one to put
/// back that is present, is a [`RuneModelError`]. Each rune in the counter
/// log above `agreed` gets back its totals from before the lowest logged
/// height, or loses its counter when it had none. The rune event, etching and
/// counter log rows above `agreed` are deleted.
fn undo_runes(tx: &WriteTransaction, agreed: u32) -> Result<()> {
    let mut events = tx.open_table(table(cf::RUNE_EVENTS))?;
    let mut etchings = tx.open_table(table(cf::RUNE_ETCHINGS))?;
    let mut balances = tx.open_table(table(cf::RUNE_BALANCES))?;
    let mut counters = tx.open_table(table(cf::RUNE_COUNTERS))?;
    let mut counter_log = tx.open_table(table(cf::RUNE_COUNTER_LOG))?;

    let first = rune_event_key(agreed + 1, 0);
    let mut by_height = BTreeMap::<[u8; 4], Vec<RuneEvent>>::new();
    for row in events.range::<&[u8]>(first.as_slice()..)? {
        let (key, value) = row?;
        let height = key.value()[..4].try_into()?;
        by_height
            .entry(height)
            .or_default()
            .push(decode_rune_event(value.value())?);
    }
    for height_events in by_height.values().rev() {
        for tx_events in height_events.chunk_by(|a, b| a.txid == b.txid).rev() {
            let of_kind = |kind| {
                tx_events
                    .iter()
                    .filter(|event| event.kind == kind)
                    .collect::<Vec<_>>()
            };
            for created in of_kind(RuneEventKind::Output).chunk_by(|a, b| a.outpoint == b.outpoint)
            {
                if balances
                    .remove(outpoint_key(created[0])?.as_slice())?
                    .is_none()
                {
                    bail!(RuneModelError(format!(
                        "undo of {} finds no balance row for its output {:?}",
                        created[0].txid, created[0].outpoint
                    )));
                }
            }
            for spent in of_kind(RuneEventKind::Input).chunk_by(|a, b| a.outpoint == b.outpoint) {
                let pkscript = spent[0]
                    .pkscript
                    .as_deref()
                    .with_context(|| format!("input event of {} has no pkscript", spent[0].txid))?;
                let held = spent
                    .iter()
                    .map(|event| (event.rune_id.clone(), event.amount))
                    .collect::<Vec<_>>();
                if balances
                    .insert(
                        outpoint_key(spent[0])?.as_slice(),
                        encode_rune_balance_row(pkscript, &held)?.as_slice(),
                    )?
                    .is_some()
                {
                    bail!(RuneModelError(format!(
                        "undo of {} finds a balance row for its spent input {:?}",
                        spent[0].txid, spent[0].outpoint
                    )));
                }
            }
        }
    }
    events.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    etchings.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;

    let first = (agreed + 1).to_be_bytes();
    for row in counter_log.range::<&[u8]>(first.as_slice()..)?.rev() {
        let (key, value) = row?;
        let rune = &key.value()[4..];
        match decode_rune_counter_log(value.value())?.0 {
            Some(before) => {
                counters.insert(rune, encode_rune_counter(before).as_slice())?;
            }
            None => {
                counters.remove(rune)?;
            }
        }
    }
    counter_log.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    Ok(())
}

/// Store form of the outpoint of an input or output event.
fn outpoint_key(event: &RuneEvent) -> Result<[u8; OUTPOINT_LEN]> {
    let outpoint = event
        .outpoint
        .as_deref()
        .with_context(|| format!("{:?} event of {} has no outpoint", event.kind, event.txid))?;
    rune_outpoint_key(outpoint)
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
