//! Finalize: turns the raw events of heights ord has committed into
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
    std::{
        collections::{HashMap, HashSet},
        ops::RangeInclusive,
    },
};

/// Most heights finalized in one write transaction.
pub const FINALIZE_BATCH: u32 = 1000;

/// Chain data finalize reads beyond the journal and ord's index.
pub trait ChainData {
    /// The block with `hash`. When `transactions` is false only its header
    /// is read, so a header-only block will do.
    fn block(&mut self, hash: BlockHash, transactions: bool) -> Result<Block>;

    /// The value of every input of every transaction of block `hash`, by
    /// transaction position and input index. The coinbase has none.
    fn input_values(&mut self, hash: BlockHash) -> Result<Vec<Vec<u64>>>;
}

/// One inscription event of the block.
struct Movement {
    inscription_id: InscriptionId,
    /// `None` for a creation.
    old_location: Option<SatPoint>,
    /// `None` for an unbound creation.
    new_location: Option<SatPoint>,
    /// Position of the transaction that moved the inscription.
    tx_index: usize,
    /// `None` until decided, for an unbound creation.
    fee: Option<bool>,
    tracked: bool,
    /// Creation facts, for a tracked creation.
    info: Option<InscriptionInfo>,
    /// Input and pointer of the creation's envelope.
    envelope: Option<(u32, Option<u64>)>,
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

type RawTable<'txn> = redb::Table<'txn, &'static [u8], &'static [u8]>;

/// Finalizes `heights`, which start at the next unfinalized height, as ord
/// committed them: for each height its transfer records, the info rows of
/// its tracked creations and its header, then `final_tip = heights.end()`,
/// all in one durable transaction.
///
/// Only raw events logged under ord's hash for a height count, so stale rows
/// of a replaced block are ignored. A height without inscription events
/// reads only its header.
pub fn finalize_range(
    journal: &Journal,
    index: &Index,
    heights: RangeInclusive<u32>,
    chain: &mut dyn ChainData,
) -> Result<()> {
    let (first, last) = (*heights.start(), *heights.end());
    ensure!(first <= last, "empty finalize range {first}..={last}");
    ensure!(
        last - first < FINALIZE_BATCH,
        "finalize range {first}..={last} exceeds {FINALIZE_BATCH} heights"
    );
    let expected = journal.final_tip()?.map_or(0, |tip| tip + 1);
    ensure!(
        first == expected,
        "finalizing from height {first}, but the next unfinalized height is {expected}"
    );

    let tx = journal.begin_write()?;
    {
        let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
        let mut infos = tx.open_table(table(cf::INFO))?;
        let mut headers = tx.open_table(table(cf::HEADERS))?;
        let mut first_transfers = tx.open_table(table(cf::FIRST_TRANSFER))?;
        let raw_events = tx.open_table(table(cf::RAW_EVENTS))?;
        let raw_heights = tx.open_table(table(cf::RAW_HEIGHTS))?;

        for height in heights {
            let hash = index
                .block_hash(Some(height))?
                .with_context(|| format!("ord has no hash for committed height {height}"))?;
            let events = logged_events(&raw_heights, &raw_events, height, hash)?;
            let block = chain.block(hash, !events.is_empty())?;
            ensure!(
                block.block_hash() == hash,
                "block {} given for {hash} at height {height}",
                block.block_hash()
            );
            if !events.is_empty() {
                finalize_events(
                    index,
                    chain,
                    Tables {
                        transfers: &mut transfers,
                        infos: &mut infos,
                        first_transfers: &mut first_transfers,
                    },
                    &block,
                    height,
                    events,
                )?;
            }
            headers.insert(
                height.to_be_bytes().as_slice(),
                consensus::serialize(&block.header).as_slice(),
            )?;
        }
    }
    set_final_tip(&tx, Some(last))?;
    tx.commit()?;
    Ok(())
}

/// The tables one block's events write.
struct Tables<'a, 'txn> {
    transfers: &'a mut RawTable<'txn>,
    infos: &'a mut RawTable<'txn>,
    first_transfers: &'a mut RawTable<'txn>,
}

/// Writes the info rows, transfer records and first-transfer entries of one
/// block's events.
fn finalize_events(
    index: &Index,
    chain: &mut dyn ChainData,
    tables: Tables,
    block: &Block,
    height: u32,
    events: Vec<RawEvent>,
) -> Result<()> {
    let Tables {
        transfers,
        infos,
        first_transfers,
    } = tables;
    let positions = Positions::new(block);
    let mut movements = movements(index, block, &positions, events, |id| {
        let key = info_key(&id.to_string())?;
        Ok((
            infos.get(key.as_slice())?.is_some(),
            first_transfers.get(key.as_slice())?.is_some(),
        ))
    })?;
    decide_fees(chain, block, &mut movements)?;

    for movement in &movements {
        if let Some(info) = &movement.info {
            infos.insert(
                info_key(&info.inscription_id)?.as_slice(),
                compress_info(&encode_info(info)?)?.as_slice(),
            )?;
        }
        if movement.tracked && movement.old_location.is_some() {
            first_transfers.insert(
                info_key(&movement.inscription_id.to_string())?.as_slice(),
                height.to_be_bytes().as_slice(),
            )?;
        }
    }

    let located = movements
        .iter()
        .enumerate()
        .map(|(event_index, m)| Located {
            event_index,
            tx_index: m.tx_index,
            fee: m.fee == Some(true),
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
    Ok(())
}

/// Inscription events of `height` logged under `hash`, in emission order.
fn logged_events(
    raw_heights: &RawTable,
    raw_events: &RawTable,
    height: u32,
    hash: BlockHash,
) -> Result<Vec<RawEvent>> {
    let tagged = raw_heights
        .get(height.to_be_bytes().as_slice())?
        .is_some_and(|tag| tag.value() == hash.as_byte_array().as_slice());
    if !tagged {
        return Ok(Vec::new());
    }
    let first = raw_event_key(height, 0);
    let last = raw_event_key(height, u32::MAX);
    let mut events = Vec::new();
    for row in raw_events.range::<&[u8]>(first.as_slice()..=last.as_slice())? {
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
/// `stored` tells, for an inscription, whether it has an info row and a
/// first-transfer entry from an earlier height. A transfer is tracked when
/// the inscription is tracked and the transfer is its first: the fork counts
/// an inscription's location updates and records a transfer only at count 2.
/// A bound event's fee flag follows from its location; an unbound creation's
/// is left undecided.
fn movements(
    index: &Index,
    block: &Block,
    positions: &Positions,
    events: Vec<RawEvent>,
    stored: impl Fn(InscriptionId) -> Result<(bool, bool)>,
) -> Result<Vec<Movement>> {
    let coinbase = *positions.txids.first().context("block has no coinbase")?;
    let mut envelopes = HashMap::<Txid, Vec<ParsedEnvelope>>::new();
    let mut tracked_here = HashSet::new();
    let mut transferred_here = HashSet::new();
    let mut movements = Vec::with_capacity(events.len());

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
                let envelope = envelopes
                    .entry(inscription_id.txid)
                    .or_insert_with(|| ParsedEnvelope::from_transaction(&block.txdata[tx_index]))
                    .get(usize::try_from(inscription_id.index)?)
                    .with_context(|| format!("no envelope for inscription {inscription_id}"))?;
                let input = envelope.input;
                let payload = envelope.payload.clone();
                let pointer = payload.pointer();
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
                    fee: None,
                    tracked,
                    info,
                    envelope: Some((input, pointer)),
                }
            }
            RawEvent::InscriptionTransferred {
                inscription_id,
                new_location,
                old_location,
                ..
            } => {
                let (has_info, has_first_transfer) = stored(inscription_id)?;
                let tracked = (tracked_here.contains(&inscription_id) || has_info)
                    && !has_first_transfer
                    && transferred_here.insert(inscription_id);
                Movement {
                    inscription_id,
                    old_location: Some(old_location),
                    new_location: Some(new_location),
                    tx_index: positions.spender(old_location.outpoint)?,
                    fee: None,
                    tracked,
                    info: None,
                    envelope: None,
                }
            }
            _ => continue,
        };
        movement.fee = movement.new_location.map(|location| {
            location.outpoint.txid == coinbase || location.outpoint == OutPoint::null()
        });
        movements.push(movement);
    }
    Ok(movements)
}

/// Decides the fee flag of every unbound creation whose flag can move a
/// tracked event's idx: from the event order when it settles it, else from
/// the reveal transaction's input values. A fee-bound untracked event only
/// reserves an idx, so an undecided one with no tracked event numbered after
/// its slot keeps `None`.
fn decide_fees(chain: &mut dyn ChainData, block: &Block, movements: &mut [Movement]) -> Result<()> {
    let order = movements
        .iter()
        .map(|m| (m.tx_index, m.fee))
        .collect::<Vec<_>>();
    for (movement, fee) in movements.iter_mut().zip(fees_from_order(&order)?) {
        movement.fee = fee;
    }
    let numbered_after = |slot: usize| {
        let (tx_index, event_index) = (movements[slot].tx_index, slot);
        movements.iter().enumerate().any(|(i, m)| {
            m.tracked
                && (m.tx_index > tx_index
                    || (m.tx_index == tx_index && m.fee == Some(true) && i > event_index))
        })
    };
    let undecided = (0..movements.len())
        .filter(|&i| movements[i].fee.is_none() && numbered_after(i))
        .collect::<Vec<_>>();
    if undecided.is_empty() {
        return Ok(());
    }

    let hash = block.block_hash();
    let values = chain.input_values(hash)?;
    ensure!(
        values.len() == block.txdata.len(),
        "input values of block {hash} cover {} transactions, expected {}",
        values.len(),
        block.txdata.len()
    );
    for slot in undecided {
        let movement = &mut movements[slot];
        let tx = &block.txdata[movement.tx_index];
        let values = &values[movement.tx_index];
        ensure!(
            values.len() == tx.input.len(),
            "input values of {} cover {} inputs, expected {}",
            tx.compute_txid(),
            values.len(),
            tx.input.len()
        );
        let (input, pointer) = movement
            .envelope
            .context("undecided movement is not a creation")?;
        let total_output = tx.output.iter().map(|o| o.value.to_sat()).sum();
        movement.fee = Some(lands_in_fee(input, pointer, values, total_output)?);
    }
    Ok(())
}

/// A block's inscription events fit no order ord emits them in, so the
/// model of ord finalize relies on is wrong. Retrying can't fix it.
#[derive(Debug)]
pub struct EmissionOrderError(&'static str);

impl std::fmt::Display for EmissionOrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "inscription events fit no ord emission order: {}",
            self.0
        )
    }
}

impl std::error::Error for EmissionOrderError {}

/// Fee flags from the order ord emits a block's inscription events in.
///
/// ord emits the events of sats that stay in their transaction first, in
/// transaction order, then the fee-bound ones at coinbase time, in mover
/// order. Each event is `(mover position, known fee flag)`. The phase change
/// sits after every known non-fee event, at or before every known fee event,
/// and exactly at a drop in mover position. An unknown flag is decided when
/// every position the change can take puts the event on the same side.
fn fees_from_order(events: &[(usize, Option<bool>)]) -> Result<Vec<Option<bool>>> {
    let mut lo = events
        .iter()
        .rposition(|e| e.1 == Some(false))
        .map_or(0, |i| i + 1);
    let mut hi = events
        .iter()
        .position(|e| e.1 == Some(true))
        .unwrap_or(events.len());
    let drops = (1..events.len())
        .filter(|&i| events[i].0 < events[i - 1].0)
        .collect::<Vec<_>>();
    match drops[..] {
        [] => {}
        [drop] => {
            lo = lo.max(drop);
            hi = hi.min(drop);
        }
        _ => return Err(EmissionOrderError("mover position drops twice").into()),
    }
    if lo > hi {
        return Err(EmissionOrderError("a fee-bound event precedes a located one").into());
    }
    Ok(events
        .iter()
        .enumerate()
        .map(|(i, &(_, fee))| {
            fee.or(if i < lo {
                Some(false)
            } else if i >= hi {
                Some(true)
            } else {
                None
            })
        })
        .collect())
}

/// Whether ord sends a new inscription to fees: its offset, the value of the
/// inputs before its envelope's input or its pointer when that is below the
/// total output, is at or beyond the transaction's total output.
fn lands_in_fee(
    input: u32,
    pointer: Option<u64>,
    input_values: &[u64],
    total_output: u64,
) -> Result<bool> {
    let input = usize::try_from(input)?;
    ensure!(
        input < input_values.len(),
        "envelope input {input} beyond {} inputs",
        input_values.len()
    );
    let start = input_values[..input].iter().sum::<u64>();
    let offset = pointer
        .filter(|&pointer| pointer < total_output)
        .unwrap_or(start);
    Ok(offset >= total_output)
}

fn is_json(payload: &Inscription) -> bool {
    payload.body.as_deref().is_some_and(is_json_body)
}

/// Whether `body` is JSON as the fork's ord judges it: it parses, and every
/// number in it is a finite `f64`. The explicit number check keeps the answer
/// identical when cargo feature unification turns on serde_json's
/// `arbitrary_precision`, under which out-of-range numbers parse.
pub fn is_json_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let mut stack = vec![&value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Number(number) => {
                if !number.as_f64().is_some_and(f64::is_finite) {
                    return false;
                }
            }
            Value::Array(items) => stack.extend(items),
            Value::Object(map) => stack.extend(map.values()),
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    true
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
        sent_as_fee: movement.fee == Some(true),
        new_output_value,
        txid: txid.to_string(),
        new_pkscript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCATED: Option<bool> = Some(false);
    const FEE: Option<bool> = Some(true);
    const UNBOUND: Option<bool> = None;

    #[test]
    fn unbound_after_a_fee_event_is_fee_bound() {
        let events = [(1, LOCATED), (1, FEE), (2, UNBOUND)];
        assert_eq!(fees_from_order(&events).unwrap(), [LOCATED, FEE, FEE]);
    }

    #[test]
    fn unbound_before_a_located_event_stays() {
        let events = [(1, UNBOUND), (2, LOCATED), (1, FEE)];
        assert_eq!(fees_from_order(&events).unwrap(), [LOCATED, LOCATED, FEE]);
    }

    #[test]
    fn unbound_at_a_mover_drop_is_fee_bound() {
        let events = [(1, LOCATED), (3, LOCATED), (2, UNBOUND), (4, FEE)];
        assert_eq!(
            fees_from_order(&events).unwrap(),
            [LOCATED, LOCATED, FEE, FEE]
        );
    }

    #[test]
    fn unbound_between_phases_is_undecided() {
        let events = [(1, LOCATED), (2, UNBOUND), (3, FEE)];
        assert_eq!(fees_from_order(&events).unwrap(), [LOCATED, UNBOUND, FEE]);
    }

    #[test]
    fn impossible_orders_fail() {
        assert!(fees_from_order(&[(1, FEE), (2, LOCATED)]).is_err());
        assert!(fees_from_order(&[(3, LOCATED), (1, FEE), (2, LOCATED)]).is_err());
        assert!(fees_from_order(&[(3, UNBOUND), (1, UNBOUND), (0, UNBOUND)]).is_err());
    }

    #[test]
    fn offset_at_or_beyond_outputs_lands_in_fee() {
        assert!(lands_in_fee(0, None, &[1000], 0).unwrap());
        assert!(!lands_in_fee(0, None, &[1000], 1).unwrap());
        assert!(lands_in_fee(1, None, &[600, 0], 600).unwrap());
        assert!(!lands_in_fee(1, None, &[600, 0], 601).unwrap());
    }

    #[test]
    fn pointer_below_outputs_sets_offset() {
        assert!(!lands_in_fee(1, Some(10), &[600, 0], 600).unwrap());
        assert!(lands_in_fee(1, Some(600), &[600, 0], 600).unwrap());
        assert!(lands_in_fee(0, Some(5), &[600], 0).unwrap());
    }

    #[test]
    fn envelope_input_out_of_range_fails() {
        assert!(lands_in_fee(2, None, &[600, 0], 600).is_err());
    }

    #[test]
    fn json_numbers_must_be_finite_f64() {
        let huge_integer = format!("1{}", "0".repeat(400));
        assert!(!is_json_body(br#"{"a":1e400}"#));
        assert!(!is_json_body(br#"{"a":-1e400}"#));
        assert!(!is_json_body(huge_integer.as_bytes()));
        assert!(is_json_body(br#"{"a":1e-400}"#));
        assert!(is_json_body(
            br#"{"p":"brc-20","op":"transfer","tick":"ordi","amt":"1.5"}"#
        ));
        assert!(is_json_body(br#"[1, 2.5, {"n": -3}]"#));
        assert!(!is_json_body(b"not json"));
    }
}
