//! Rune finalize: turns one block's rune events into rune event records in
//! the canonical order, etchings, outpoint balances and counter rows.

use {
    super::{
        decode_rune_balance_row, decode_rune_counter, encode_rune_balance_row, encode_rune_counter,
        encode_rune_counter_log, encode_rune_etching, encode_rune_event, finalize::RawTable,
        rune_counter_log_key, rune_etching_key, rune_event_key, rune_id_key, rune_outpoint_key,
    },
    crate::raw_event::RawEvent,
    anyhow::{Result, bail, ensure},
    bitcoin::{Block, OutPoint, Transaction, Txid},
    opi_core::{RuneEtching, RuneEvent, RuneEventKind, RuneTerms},
    ord::{Index, RuneEntry},
    ordinals::{Artifact, RuneId, Runestone},
    redb::ReadableTable,
    std::collections::{BTreeMap, HashMap},
};

/// A block's rune events break the model of ord's rune emission finalize
/// relies on. Retrying can't fix it.
#[derive(Debug)]
pub struct RuneModelError(pub(crate) String);

impl std::fmt::Display for RuneModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "rune events fit no ord emission: {}", self.0)
    }
}

impl std::error::Error for RuneModelError {}

/// The tables rune finalize writes.
pub(super) struct RuneTables<'a, 'txn> {
    pub events: &'a mut RawTable<'txn>,
    pub etchings: &'a mut RawTable<'txn>,
    pub balances: &'a mut RawTable<'txn>,
    pub counters: &'a mut RawTable<'txn>,
    pub counter_log: &'a mut RawTable<'txn>,
}

/// ord's rune events of one transaction, in emission order.
#[derive(Default)]
struct TxRunes {
    etched: Option<RuneId>,
    mints: Vec<(RuneId, u128)>,
    transfers: Vec<(OutPoint, RuneId, u128)>,
    burns: Vec<(RuneId, u128)>,
}

/// Burned amount and mint count a block adds to a rune.
#[derive(Default)]
struct CounterDelta {
    burned: u128,
    mints: u128,
}

/// Writes the rune records of `block` at `height` from its logged `events`:
/// per transaction in block order, its input, allocation, mint, output and
/// burn events, its etching, and its spent and created outpoint balances;
/// then one counter row per rune burned or minted in the block, in rune id
/// order.
///
/// ord emits an output or burn event for every rune a transaction's inputs
/// hold, so a transaction without rune events spends no rune outpoint and is
/// skipped.
pub(super) fn finalize_runes(
    index: &Index,
    tables: RuneTables,
    block: &Block,
    height: u32,
    events: &[RawEvent],
) -> Result<()> {
    let mut by_txid = HashMap::<Txid, TxRunes>::new();
    for event in events {
        match *event {
            RawEvent::RuneEtched { rune_id, txid, .. } => {
                let tx = by_txid.entry(txid).or_default();
                ensure!(
                    tx.etched.is_none(),
                    RuneModelError(format!("transaction {txid} etches twice"))
                );
                tx.etched = Some(rune_id);
            }
            RawEvent::RuneMinted {
                rune_id,
                txid,
                amount,
                ..
            } => by_txid
                .entry(txid)
                .or_default()
                .mints
                .push((rune_id, amount)),
            RawEvent::RuneTransferred {
                rune_id,
                txid,
                amount,
                outpoint,
                ..
            } => by_txid
                .entry(txid)
                .or_default()
                .transfers
                .push((outpoint, rune_id, amount)),
            RawEvent::RuneBurned {
                rune_id,
                txid,
                amount,
                ..
            } => by_txid
                .entry(txid)
                .or_default()
                .burns
                .push((rune_id, amount)),
            RawEvent::InscriptionCreated { .. } | RawEvent::InscriptionTransferred { .. } => {}
        }
    }
    if by_txid.is_empty() {
        return Ok(());
    }

    let mut ordered = BTreeMap::new();
    for (position, tx) in block.txdata.iter().enumerate() {
        let txid = tx.compute_txid();
        if let Some(runes) = by_txid.remove(&txid) {
            ordered.insert(position, (tx, txid, runes));
        }
    }
    if let Some(txid) = by_txid.keys().next() {
        bail!(RuneModelError(format!(
            "rune events of transaction {txid} not in block {height}"
        )));
    }

    let RuneTables {
        events: event_table,
        etchings,
        balances,
        counters,
        counter_log,
    } = tables;
    let time = u64::from(block.header.time);
    let mut records = Vec::new();
    let mut etching_seq = 0;
    let mut deltas = BTreeMap::<RuneId, CounterDelta>::new();

    for (tx, txid, runes) in ordered.into_values() {
        let etching = runes
            .etched
            .map(|id| etching(index, id, txid, time))
            .transpose()?;
        records.extend(transaction_events(
            balances,
            tx,
            txid,
            &runes,
            etching.as_ref(),
        )?);
        if let Some(etching) = etching {
            etchings.insert(
                rune_etching_key(height, etching_seq).as_slice(),
                encode_rune_etching(&etching)?.as_slice(),
            )?;
            etching_seq += 1;
        }
        for (id, _) in &runes.mints {
            deltas.entry(*id).or_default().mints += 1;
        }
        for (id, amount) in &runes.burns {
            let delta = deltas.entry(*id).or_default();
            delta.burned = delta.burned.checked_add(*amount).ok_or_else(|| {
                RuneModelError(format!(
                    "burned amount of rune {id} in block {height} overflows"
                ))
            })?;
        }
    }

    for (seq, record) in records.iter().enumerate() {
        event_table.insert(
            rune_event_key(height, u32::try_from(seq)?).as_slice(),
            encode_rune_event(record)?.as_slice(),
        )?;
    }

    for (id, delta) in deltas {
        let id = id.to_string();
        let key = rune_id_key(&id)?;
        let before = counters
            .get(key.as_slice())?
            .map(|totals| decode_rune_counter(totals.value()))
            .transpose()?;
        let (burned, mints) = before.unwrap_or_default();
        let after = (
            burned
                .checked_add(delta.burned)
                .ok_or_else(|| RuneModelError(format!("burned total of rune {id} overflows")))?,
            mints
                .checked_add(delta.mints)
                .ok_or_else(|| RuneModelError(format!("mint total of rune {id} overflows")))?,
        );
        counters.insert(key.as_slice(), encode_rune_counter(after).as_slice())?;
        counter_log.insert(
            rune_counter_log_key(height, &id)?.as_slice(),
            encode_rune_counter_log(before, after).as_slice(),
        )?;
    }
    Ok(())
}

/// The events of one transaction in canonical order, spending its rune
/// inputs from `balances` and storing its rune outputs there. Fails when the
/// events break rune conservation, which means ord emitted something this
/// model does not expect.
fn transaction_events(
    balances: &mut RawTable,
    tx: &Transaction,
    txid: Txid,
    runes: &TxRunes,
    etching: Option<&RuneEtching>,
) -> Result<Vec<RuneEvent>> {
    let txid_string = txid.to_string();
    let event = |kind, outpoint: Option<OutPoint>, pkscript, rune_id: String, amount| RuneEvent {
        kind,
        txid: txid_string.clone(),
        outpoint: outpoint.map(|outpoint| outpoint.to_string()),
        pkscript,
        rune_id,
        amount,
    };
    let mut events = Vec::new();
    // Per rune: amount in (inputs, allocation, mints), amount out (outputs,
    // burns).
    let mut flow = BTreeMap::<String, (u128, u128)>::new();
    let mut add = |rune_id: &str, amount: u128, incoming: bool| -> Result<()> {
        let (into, out) = flow.entry(rune_id.to_owned()).or_default();
        let side = if incoming { into } else { out };
        *side = side
            .checked_add(amount)
            .ok_or_else(|| RuneModelError(format!("rune {rune_id} amounts of {txid} overflow")))?;
        Ok(())
    };

    for input in &tx.input {
        let spent = input.previous_output;
        let Some(row) = balances.remove(rune_outpoint_key(&spent.to_string())?.as_slice())? else {
            continue;
        };
        let (pkscript, held) = decode_rune_balance_row(row.value())?;
        drop(row);
        for (rune_id, amount) in held {
            add(&rune_id, amount, true)?;
            events.push(event(
                RuneEventKind::Input,
                Some(spent),
                Some(pkscript.clone()),
                rune_id,
                amount,
            ));
        }
    }

    if let Some(etching) = etching
        && matches!(Runestone::decipher(tx), Some(Artifact::Runestone(_)))
    {
        add(&etching.rune_id, etching.premine, true)?;
        events.push(event(
            RuneEventKind::NewAllocation,
            None,
            None,
            etching.rune_id.clone(),
            etching.premine,
        ));
    }

    for (id, amount) in &runes.mints {
        let rune_id = id.to_string();
        add(&rune_id, *amount, true)?;
        events.push(event(RuneEventKind::Mint, None, None, rune_id, *amount));
    }

    let mut rows = Vec::<(OutPoint, Vec<u8>, Vec<(String, u128)>)>::new();
    let mut last = None;
    for (outpoint, id, amount) in &runes.transfers {
        ensure!(
            outpoint.txid == txid && last < Some((outpoint.vout, *id)),
            RuneModelError(format!(
                "rune transfers of {txid} are not in vout and rune id order"
            ))
        );
        last = Some((outpoint.vout, *id));
        let pkscript = tx
            .output
            .get(usize::try_from(outpoint.vout)?)
            .ok_or_else(|| RuneModelError(format!("{txid} has no output {}", outpoint.vout)))?
            .script_pubkey
            .to_bytes();
        let rune_id = id.to_string();
        add(&rune_id, *amount, false)?;
        events.push(event(
            RuneEventKind::Output,
            Some(*outpoint),
            Some(pkscript.clone()),
            rune_id.clone(),
            *amount,
        ));
        match rows.last_mut() {
            Some((row_outpoint, _, held)) if row_outpoint == outpoint => {
                held.push((rune_id, *amount));
            }
            _ => rows.push((*outpoint, pkscript, vec![(rune_id, *amount)])),
        }
    }

    let mut burns = runes.burns.clone();
    burns.sort_unstable_by_key(|(id, _)| *id);
    for (id, amount) in burns {
        let rune_id = id.to_string();
        add(&rune_id, amount, false)?;
        events.push(event(RuneEventKind::Burn, None, None, rune_id, amount));
    }

    if let Some((rune_id, (into, out))) = flow.iter().find(|(_, (into, out))| into != out) {
        bail!(RuneModelError(format!(
            "rune {rune_id} of {txid}: {into} in, {out} out"
        )));
    }

    for (outpoint, pkscript, held) in rows {
        balances.insert(
            rune_outpoint_key(&outpoint.to_string())?.as_slice(),
            encode_rune_balance_row(&pkscript, &held)?.as_slice(),
        )?;
    }
    Ok(events)
}

/// The etching record of rune `id`, etched by `txid` in a block with header
/// time `time`, from ord's entry for it. Finalize runs after ord commits the
/// height, so a missing rune or entry is a [`RuneModelError`].
fn etching(index: &Index, id: RuneId, txid: Txid, time: u64) -> Result<RuneEtching> {
    let Some(rune) = index.get_rune_by_id(id)? else {
        bail!(RuneModelError(format!("ord has no rune {id}")));
    };
    let entry = index
        .rune(rune)?
        .map(|(entry_id, entry, _)| (entry_id, entry));
    etching_record(id, txid, time, entry)
}

/// The etching record of rune `id` from ord's `(id, entry)` for it. Only the
/// entry's immutable fields are used: its burned and mint totals are those of
/// ord's committed tip.
fn etching_record(
    id: RuneId,
    txid: Txid,
    time: u64,
    entry: Option<(RuneId, RuneEntry)>,
) -> Result<RuneEtching> {
    let Some((entry_id, entry)) = entry else {
        bail!(RuneModelError(format!("ord has no entry for rune {id}")));
    };
    let rune = entry.spaced_rune.rune;
    ensure!(
        entry_id == id && entry.etching == txid,
        RuneModelError(format!(
            "ord's entry for rune {rune} is {entry_id} etched by {}, expected {id} etched by \
             {txid}",
            entry.etching
        ))
    );
    Ok(RuneEtching {
        rune_id: id.to_string(),
        block: entry.block,
        divisibility: entry.divisibility,
        etching: txid.to_string(),
        terms: entry.terms.map(|terms| RuneTerms {
            amount: terms.amount,
            cap: terms.cap,
            height: terms.height,
            offset: terms.offset,
        }),
        number: entry.number,
        premine: entry.premine,
        rune: entry.spaced_rune.rune.to_string(),
        spacers: entry.spaced_rune.spacers,
        symbol: entry.symbol,
        timestamp: time,
        turbo: entry.turbo,
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::journal::{cf, table},
        bitcoin::{Amount, ScriptBuf, TxOut, absolute::LockTime, transaction::Version},
        redb::{Database, backends::InMemoryBackend},
    };

    const RUNE: RuneId = RuneId { block: 1, tx: 0 };

    fn transaction(outputs: usize) -> Transaction {
        Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: Vec::new(),
            output: (0..outputs)
                .map(|_| TxOut {
                    value: Amount::from_sat(1000),
                    script_pubkey: ScriptBuf::new(),
                })
                .collect(),
        }
    }

    /// Runs [`transaction_events`] for `runes` of a `outputs`-output
    /// transaction against an empty balances table.
    fn events_of(outputs: usize, runes: TxRunes) -> Result<Vec<RuneEvent>> {
        let db = Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap();
        let wtx = db.begin_write().unwrap();
        let mut balances = wtx.open_table(table(cf::RUNE_BALANCES)).unwrap();
        let tx = transaction(outputs);
        transaction_events(&mut balances, &tx, tx.compute_txid(), &runes, None)
    }

    fn is_model_error(result: Result<Vec<RuneEvent>>) -> bool {
        result
            .unwrap_err()
            .chain()
            .any(|cause| cause.is::<RuneModelError>())
    }

    #[test]
    fn unbalanced_transaction_is_a_model_error() {
        let runes = TxRunes {
            mints: vec![(RUNE, 5)],
            ..TxRunes::default()
        };
        assert!(is_model_error(events_of(1, runes)));
    }

    #[test]
    fn transfers_out_of_vout_order_are_a_model_error() {
        let txid = transaction(2).compute_txid();
        let runes = TxRunes {
            mints: vec![(RUNE, 2)],
            transfers: vec![
                (OutPoint { txid, vout: 1 }, RUNE, 1),
                (OutPoint { txid, vout: 0 }, RUNE, 1),
            ],
            ..TxRunes::default()
        };
        assert!(is_model_error(events_of(2, runes)));
    }

    #[test]
    fn transfer_to_a_missing_output_is_a_model_error() {
        let txid = transaction(1).compute_txid();
        let runes = TxRunes {
            mints: vec![(RUNE, 1)],
            transfers: vec![(OutPoint { txid, vout: 3 }, RUNE, 1)],
            ..TxRunes::default()
        };
        assert!(is_model_error(events_of(1, runes)));
    }

    #[test]
    fn amount_overflow_is_a_model_error() {
        let runes = TxRunes {
            mints: vec![(RUNE, u128::MAX), (RUNE, 1)],
            ..TxRunes::default()
        };
        assert!(is_model_error(events_of(1, runes)));
    }

    #[test]
    fn missing_entry_is_a_model_error() {
        let txid = transaction(1).compute_txid();
        let error = etching_record(RUNE, txid, 0, None).unwrap_err();
        assert!(error.chain().any(|cause| cause.is::<RuneModelError>()));
    }

    #[test]
    fn balanced_transaction_passes() {
        let txid = transaction(1).compute_txid();
        let runes = TxRunes {
            mints: vec![(RUNE, 3)],
            transfers: vec![(OutPoint { txid, vout: 0 }, RUNE, 3)],
            ..TxRunes::default()
        };
        assert_eq!(events_of(1, runes).unwrap().len(), 2);
    }
}
