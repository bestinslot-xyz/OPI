//! Finalize: turns the raw events of one height ord has committed into
//! fork-compatible transfer and inscription info records.

use {
    super::{
        Journal, cf, compress_info, encode_info, encode_transfer, info_key,
        order::{Located, assign_idx},
        raw_event_key, set_final_tip, table, transfer_key,
    },
    crate::raw_event::RawEvent,
    anyhow::{Context, Result, ensure},
    bitcoin::{Block, BlockHash, OutPoint, Txid, consensus, hashes::Hash},
    opi_core::{InscriptionInfo, InscriptionRecord},
    ord::{Index, Inscription, InscriptionId, ParsedEnvelope},
    ordinals::{Charm, SatPoint},
    redb::ReadableTable,
    serde_json::Value,
    std::collections::{HashMap, HashSet},
};

/// One inscription event of the block.
struct Movement {
    inscription_id: InscriptionId,
    /// `None` for a creation.
    old_location: Option<SatPoint>,
    /// `None` for an unbound creation.
    new_location: Option<SatPoint>,
    /// Position of the transaction that moved the inscription.
    tx_index: usize,
    fee: bool,
    tracked: bool,
    /// Creation facts, for a tracked creation.
    info: Option<InscriptionInfo>,
}

/// Transaction positions within a block.
struct Positions {
    txids: Vec<Txid>,
    by_txid: HashMap<Txid, usize>,
    by_spent: HashMap<OutPoint, usize>,
}

impl Positions {
    fn new(block: &Block) -> Positions {
        let txids = block
            .txdata
            .iter()
            .map(|tx| tx.compute_txid())
            .collect::<Vec<_>>();
        let by_txid = txids.iter().enumerate().map(|(i, t)| (*t, i)).collect();
        let by_spent = block
            .txdata
            .iter()
            .enumerate()
            .skip(1)
            .flat_map(|(i, tx)| tx.input.iter().map(move |input| (input.previous_output, i)))
            .collect();
        Positions {
            txids,
            by_txid,
            by_spent,
        }
    }

    fn of(&self, txid: Txid) -> Result<usize> {
        self.by_txid
            .get(&txid)
            .copied()
            .with_context(|| format!("transaction {txid} is not in the block"))
    }

    fn spender(&self, outpoint: OutPoint) -> Result<usize> {
        self.by_spent
            .get(&outpoint)
            .copied()
            .with_context(|| format!("no transaction in the block spends {outpoint}"))
    }
}

/// Finalizes `height`, whose block is `block` with hash `hash` as ord
/// committed it: writes its transfer records, the info rows of its tracked
/// creations, its header and `final_tip = height` in one durable transaction.
///
/// Only raw events logged under `hash` count, so stale rows of a replaced
/// block are ignored. A height with no events still gets its header. The
/// block needs its transactions only when the height has inscription events.
pub fn finalize_height(
    journal: &Journal,
    index: &Index,
    block: &Block,
    height: u32,
    hash: BlockHash,
) -> Result<()> {
    ensure!(
        block.block_hash() == hash,
        "block {} given for {hash} at height {height}",
        block.block_hash()
    );
    let expected = journal.final_tip()?.map_or(0, |tip| tip + 1);
    ensure!(
        height == expected,
        "finalizing height {height}, but the next unfinalized height is {expected}"
    );

    let events = raw_events(journal, height, hash)?;

    let tx = journal.begin_write()?;
    {
        let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
        let mut infos = tx.open_table(table(cf::INFO))?;
        let mut headers = tx.open_table(table(cf::HEADERS))?;

        if !events.is_empty() {
            let positions = Positions::new(block);
            let movements = movements(index, block, &positions, events, |id| {
                Ok(infos.get(info_key(&id.to_string())?.as_slice())?.is_some())
            })?;

            for movement in &movements {
                if let Some(info) = &movement.info {
                    infos.insert(
                        info_key(&info.inscription_id)?.as_slice(),
                        compress_info(&encode_info(info)?)?.as_slice(),
                    )?;
                }
            }

            let located = movements
                .iter()
                .enumerate()
                .map(|(event_index, m)| Located {
                    event_index,
                    tx_index: m.tx_index,
                    fee: m.fee,
                    tracked: m.tracked,
                })
                .collect::<Vec<_>>();
            for (event_index, idx) in assign_idx(&located) {
                let record = record(block, &positions, height, idx, &movements[event_index])?;
                transfers.insert(
                    transfer_key(height, idx).as_slice(),
                    encode_transfer(&record)?.as_slice(),
                )?;
            }
        }

        headers.insert(
            height.to_be_bytes().as_slice(),
            consensus::serialize(&block.header).as_slice(),
        )?;
    }
    set_final_tip(&tx, Some(height))?;
    tx.commit()?;
    Ok(())
}

/// Whether finalizing `height` under `hash` reads the block's transactions,
/// that is, whether the raw log holds inscription events for it.
pub(crate) fn needs_transactions(journal: &Journal, height: u32, hash: BlockHash) -> Result<bool> {
    Ok(!raw_events(journal, height, hash)?.is_empty())
}

/// Inscription events of `height` logged under `hash`, in emission order.
fn raw_events(journal: &Journal, height: u32, hash: BlockHash) -> Result<Vec<RawEvent>> {
    let tx = journal.db().begin_read()?;
    let tagged = tx
        .open_table(table(cf::RAW_HEIGHTS))?
        .get(height.to_be_bytes().as_slice())?
        .is_some_and(|tag| tag.value() == hash.as_byte_array().as_slice());
    if !tagged {
        return Ok(Vec::new());
    }
    let first = raw_event_key(height, 0);
    let last = raw_event_key(height, u32::MAX);
    let mut events = Vec::new();
    for row in tx
        .open_table(table(cf::RAW_EVENTS))?
        .range::<&[u8]>(first.as_slice()..=last.as_slice())?
    {
        let event = RawEvent::decode(row?.1.value())?;
        if matches!(
            event,
            RawEvent::InscriptionCreated { .. } | RawEvent::InscriptionTransferred { .. }
        ) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Places every inscription event in the block and decides what it tracks.
/// `has_info` tells whether an inscription created at an earlier height has
/// an info row.
fn movements(
    index: &Index,
    block: &Block,
    positions: &Positions,
    events: Vec<RawEvent>,
    has_info: impl Fn(InscriptionId) -> Result<bool>,
) -> Result<Vec<Movement>> {
    let coinbase = *positions.txids.first().context("block has no coinbase")?;
    let mut envelopes = HashMap::<Txid, Vec<ParsedEnvelope>>::new();
    let mut tracked_here = HashSet::new();
    let mut movements = Vec::with_capacity(events.len());
    // ord emits a block's located events in transaction order, then its
    // fee-bound events in mover order at coinbase time. An unbound creation
    // has no location to tell its phase, so it is fee-bound when a fee-bound
    // event precedes it or its mover comes before the previous event's.
    let mut fee_phase = false;
    let mut previous_tx = 0;

    for event in events {
        let mut movement = match event {
            RawEvent::InscriptionCreated {
                charms,
                inscription_id,
                location,
                parent_inscription_ids,
                ..
            } => {
                let tx_index = positions.of(inscription_id.txid)?;
                let payload = envelopes
                    .entry(inscription_id.txid)
                    .or_insert_with(|| ParsedEnvelope::from_transaction(&block.txdata[tx_index]))
                    .get(usize::try_from(inscription_id.index)?)
                    .map(|envelope| envelope.payload.clone())
                    .with_context(|| format!("no envelope for inscription {inscription_id}"))?;
                let is_json = is_json(&payload);
                let tracked = location.is_some() && (is_json || is_text(&payload));
                let info = if tracked {
                    tracked_here.insert(inscription_id);
                    Some(info(
                        index,
                        inscription_id,
                        charms,
                        &parent_inscription_ids,
                        is_json,
                        payload,
                    )?)
                } else {
                    None
                };
                Movement {
                    inscription_id,
                    old_location: None,
                    new_location: location,
                    tx_index,
                    fee: false,
                    tracked,
                    info,
                }
            }
            RawEvent::InscriptionTransferred {
                inscription_id,
                new_location,
                old_location,
                ..
            } => Movement {
                inscription_id,
                old_location: Some(old_location),
                new_location: Some(new_location),
                tx_index: positions.spender(old_location.outpoint)?,
                fee: false,
                tracked: tracked_here.contains(&inscription_id) || has_info(inscription_id)?,
                info: None,
            },
            _ => continue,
        };
        movement.fee = match movement.new_location {
            Some(location) => {
                location.outpoint.txid == coinbase || location.outpoint == OutPoint::null()
            }
            None => fee_phase || movement.tx_index < previous_tx,
        };
        fee_phase |= movement.fee;
        previous_tx = movement.tx_index;
        movements.push(movement);
    }
    Ok(movements)
}

fn is_json(payload: &Inscription) -> bool {
    payload
        .body
        .as_deref()
        .is_some_and(|body| serde_json::from_slice::<Value>(body).is_ok())
}

fn is_text(payload: &Inscription) -> bool {
    let content_type = payload.content_type.as_deref().unwrap_or_default();
    let content_type = std::str::from_utf8(content_type).unwrap_or_default();
    ["text/plain", "application/json"].iter().any(|tracked| {
        content_type
            .strip_prefix(tracked)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(';'))
    })
}

fn info(
    index: &Index,
    inscription_id: InscriptionId,
    charms: u16,
    parents: &[InscriptionId],
    is_json: bool,
    payload: Inscription,
) -> Result<InscriptionInfo> {
    let entry = index
        .get_inscription_entry(inscription_id)?
        .with_context(|| format!("ord has no entry for inscription {inscription_id}"))?;
    Ok(InscriptionInfo {
        inscription_id: inscription_id.to_string(),
        inscription_number: entry.inscription_number,
        cursed_for_brc20: Charm::Cursed.is_set(charms) || Charm::Vindicated.is_set(charms),
        parent_id: parents.first().map(ToString::to_string),
        is_json,
        content: payload.body.unwrap_or_default(),
        content_type: payload.content_type.unwrap_or_default(),
        metaprotocol: payload.metaprotocol.unwrap_or_default(),
    })
}

/// The transfer record of a tracked movement numbered `idx`.
fn record(
    block: &Block,
    positions: &Positions,
    height: u32,
    idx: u32,
    movement: &Movement,
) -> Result<InscriptionRecord> {
    let new_location = movement
        .new_location
        .context("tracked movement has no location")?;
    let (new_pkscript, new_output_value) = if new_location.outpoint == OutPoint::null() {
        (Vec::new(), 0)
    } else {
        let tx = &block.txdata[positions.of(new_location.outpoint.txid)?];
        let output = tx
            .output
            .get(usize::try_from(new_location.outpoint.vout)?)
            .with_context(|| format!("block has no output {}", new_location.outpoint))?;
        (output.script_pubkey.to_bytes(), output.value.to_sat())
    };
    let txid = match movement.old_location {
        None => movement.inscription_id.txid,
        Some(_) => positions.txids[movement.tx_index],
    };
    Ok(InscriptionRecord {
        height,
        idx,
        inscription_id: movement.inscription_id.to_string(),
        old_satpoint: movement.old_location.map(|location| location.to_string()),
        new_satpoint: new_location.to_string(),
        sent_as_fee: movement.fee,
        new_output_value,
        txid: txid.to_string(),
        new_pkscript,
    })
}
