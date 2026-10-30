use std::collections::{BTreeMap, HashMap};

use bitcoin::{Address, Network, Script};
use opi_core::{RuneEvent, RuneEventKind};
use sha2::{Digest, Sha256};

pub const EVENT_SEPARATOR: &str = "|";

/// Largest integer a JavaScript number holds exactly, `2^53 - 1`.
pub const JS_MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// `<type>;<outpoint or "">;<pkscript hex or "">;<rune_id>;<amount>`.
pub fn event_str(event: &RuneEvent) -> String {
    format!(
        "{};{};{};{};{}",
        event.kind.name(),
        event.outpoint.as_deref().unwrap_or(""),
        event
            .pkscript
            .as_deref()
            .map(hex::encode)
            .unwrap_or_default(),
        event.rune_id,
        event.amount
    )
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `sha256` of the events joined with `|`; an empty block hashes `""`.
pub fn block_event_hash(events: &[String]) -> String {
    sha256_hex(&events.join(EVENT_SEPARATOR))
}

/// Chains the block hash onto the previous height's cumulative hash, which is
/// absent at the first hashed height.
pub fn cumulative_hash(previous: Option<&str>, block_event_hash: &str) -> String {
    match previous {
        None => block_event_hash.to_string(),
        Some(previous) => sha256_hex(&format!("{previous}{block_event_hash}")),
    }
}

/// Hex of the symbol's UTF-8 bytes zero-padded to 4 bytes, e.g. `$` is
/// `24000000`.
pub fn symbol_hex(symbol: char) -> String {
    let mut bytes = [0u8; 4];
    symbol.encode_utf8(&mut bytes);
    hex::encode(bytes)
}

/// Address of a p2pkh, p2sh, p2wpkh, p2wsh or p2tr script on `network`
/// (`bc`, `tb` for testnet, testnet4 and signet, `bcrt`); `None` for every
/// other script. A p2tr program is encoded whether or not it is a valid
/// x-only key.
pub fn wallet_addr(pkscript: &[u8], network: &str) -> Option<String> {
    let network = match network {
        "mainnet" => Network::Bitcoin,
        "testnet" | "testnet4" | "signet" => Network::Testnet,
        "regtest" => Network::Regtest,
        _ => return None,
    };
    let script = Script::from_bytes(pkscript);
    let known = script.is_p2pkh()
        || script.is_p2sh()
        || script.is_p2wpkh()
        || script.is_p2wsh()
        || script.is_p2tr();
    if !known {
        return None;
    }
    Address::from_script(script, network)
        .ok()
        .map(|address| address.to_string())
}

/// A terms height or offset as stored: values from `2^53` up become
/// `2^53 - 1`.
pub fn js_safe(value: u64) -> i64 {
    i64::try_from(value)
        .unwrap_or(i64::MAX)
        .min(JS_MAX_SAFE_INTEGER)
}

/// One `runes_outpoint_to_balances` row: an output outpoint with its
/// balances in rune-id order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutpointRow {
    pub outpoint: String,
    pub pkscript: Vec<u8>,
    pub rune_ids: Vec<String>,
    pub balances: Vec<u128>,
}

/// Groups the contiguous output events of each outpoint into one row, in
/// event order. An output event without an outpoint or pkscript is an error.
pub fn outpoint_rows(events: &[RuneEvent]) -> anyhow::Result<Vec<OutpointRow>> {
    let mut rows: Vec<OutpointRow> = Vec::new();
    for event in events.iter().filter(|e| e.kind == RuneEventKind::Output) {
        let (Some(outpoint), Some(pkscript)) = (&event.outpoint, &event.pkscript) else {
            anyhow::bail!("output event of {} has no outpoint or pkscript", event.txid);
        };
        match rows.last_mut() {
            Some(row) if row.outpoint == *outpoint => {
                row.rune_ids.push(event.rune_id.clone());
                row.balances.push(event.amount);
            }
            _ => rows.push(OutpointRow {
                outpoint: outpoint.clone(),
                pkscript: pkscript.clone(),
                rune_ids: vec![event.rune_id.clone()],
                balances: vec![event.amount],
            }),
        }
    }
    Ok(rows)
}

/// The distinct outpoints the block's input events spend, in event order.
/// An input event without an outpoint is an error.
pub fn spent_outpoints(events: &[RuneEvent]) -> anyhow::Result<Vec<String>> {
    let mut spent: Vec<String> = Vec::new();
    for event in events.iter().filter(|e| e.kind == RuneEventKind::Input) {
        let Some(outpoint) = &event.outpoint else {
            anyhow::bail!("input event of {} has no outpoint", event.txid);
        };
        if spent.last() != Some(outpoint) {
            spent.push(outpoint.clone());
        }
    }
    Ok(spent)
}

/// What an unspent outpoint holds: its pkscript and its `(rune id, amount)`
/// balances in rune-id order.
pub type Held = (Vec<u8>, Vec<(String, u128)>);

/// The block's events with the input events in place: each transaction's
/// events start with one input event per rune of each outpoint it spends
/// that `held` knows, in input order, followed by the journal's events of
/// the transaction. Every transaction `spent` names must have journal events,
/// and every transaction must take in what it puts out, per rune; anything
/// else means the journal and the module's outpoints disagree.
pub fn complete_events(
    events: &[RuneEvent],
    spent: &[opi_core::RuneSpend],
    held: &HashMap<String, Held>,
) -> anyhow::Result<Vec<RuneEvent>> {
    let mut spends: HashMap<&str, &[String]> = spent
        .iter()
        .map(|spend| (spend.txid.as_str(), spend.outpoints.as_slice()))
        .collect();
    let mut complete = Vec::with_capacity(events.len() + spent.len());
    for tx_events in events.chunk_by(|a, b| a.txid == b.txid) {
        let txid = &tx_events[0].txid;
        let start = complete.len();
        for outpoint in spends.remove(txid.as_str()).unwrap_or_default() {
            let Some((pkscript, balances)) = held.get(outpoint) else {
                continue;
            };
            for (rune_id, amount) in balances {
                complete.push(RuneEvent {
                    kind: RuneEventKind::Input,
                    txid: txid.clone(),
                    outpoint: Some(outpoint.clone()),
                    pkscript: Some(pkscript.clone()),
                    rune_id: rune_id.clone(),
                    amount: *amount,
                });
            }
        }
        complete.extend_from_slice(tx_events);
        check_conservation(&complete[start..])?;
    }
    // A transaction that only etches a rune with nothing to allocate has no
    // events; it spends no rune outpoint, since ord would emit its outputs or
    // burns.
    for (txid, outpoints) in spends {
        if let Some(outpoint) = outpoints
            .iter()
            .find(|outpoint| held.contains_key(*outpoint))
        {
            anyhow::bail!("{txid} spends rune outpoint {outpoint} but has no rune events");
        }
    }
    Ok(complete)
}

/// Fails unless one transaction's events take in, per rune, what they put
/// out: inputs, allocation and mints against outputs and burns.
fn check_conservation(events: &[RuneEvent]) -> anyhow::Result<()> {
    let mut flow = BTreeMap::<&str, (u128, u128)>::new();
    for event in events {
        let (into, out) = flow.entry(event.rune_id.as_str()).or_default();
        let side = match event.kind {
            RuneEventKind::Input | RuneEventKind::NewAllocation | RuneEventKind::Mint => into,
            RuneEventKind::Output | RuneEventKind::Burn => out,
        };
        *side = side.checked_add(event.amount).ok_or_else(|| {
            anyhow::anyhow!("rune {} amounts of {} overflow", event.rune_id, event.txid)
        })?;
    }
    if let Some((rune_id, (into, out))) = flow.iter().find(|(_, (into, out))| into != out) {
        anyhow::bail!(
            "rune {rune_id} of {}: {into} in, {out} out; the module's outpoint balances \
             disagree with the journal",
            events[0].txid
        );
    }
    Ok(())
}

/// One counter row per rune the events mint or burn, in rune id order:
/// its totals in `before` (zero when absent) plus the block's burned amount
/// and mint count.
pub fn counters(
    events: &[RuneEvent],
    before: &HashMap<String, (u128, u128)>,
) -> anyhow::Result<Vec<opi_core::RuneCounter>> {
    let mut deltas = BTreeMap::<(u64, u32), (&str, u128, u128)>::new();
    for event in events {
        let mint = match event.kind {
            RuneEventKind::Mint => true,
            RuneEventKind::Burn => false,
            _ => continue,
        };
        let delta =
            deltas
                .entry(rune_id_order(&event.rune_id)?)
                .or_insert((event.rune_id.as_str(), 0, 0));
        if mint {
            delta.2 += 1;
        } else {
            delta.1 = delta.1.checked_add(event.amount).ok_or_else(|| {
                anyhow::anyhow!("burned amount of rune {} overflows", event.rune_id)
            })?;
        }
    }
    deltas
        .into_values()
        .map(|(rune_id, burned, mints)| {
            let (total_burned, total_mints) = before.get(rune_id).copied().unwrap_or_default();
            Ok(opi_core::RuneCounter {
                rune_id: rune_id.to_string(),
                burned: total_burned
                    .checked_add(burned)
                    .ok_or_else(|| anyhow::anyhow!("burned total of rune {rune_id} overflows"))?,
                mints: total_mints
                    .checked_add(mints)
                    .ok_or_else(|| anyhow::anyhow!("mint total of rune {rune_id} overflows"))?,
            })
        })
        .collect()
}

/// A `block:tx` rune id as numbers, for ordering.
fn rune_id_order(rune_id: &str) -> anyhow::Result<(u64, u32)> {
    let invalid = || anyhow::anyhow!("invalid rune id {rune_id}");
    let (block, tx) = rune_id.split_once(':').ok_or_else(invalid)?;
    Ok((
        block.parse().map_err(|_| invalid())?,
        tx.parse().map_err(|_| invalid())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(outpoint: &str, rune_id: &str, amount: u128) -> RuneEvent {
        RuneEvent {
            kind: RuneEventKind::Output,
            txid: "t".into(),
            outpoint: Some(outpoint.into()),
            pkscript: Some(vec![0x51]),
            rune_id: rune_id.into(),
            amount,
        }
    }

    #[test]
    fn terms_values_cap_at_the_js_safe_integer() {
        assert_eq!(js_safe(10), 10);
        assert_eq!(js_safe((1 << 53) - 1), JS_MAX_SAFE_INTEGER);
        assert_eq!(js_safe(1 << 53), JS_MAX_SAFE_INTEGER);
        assert_eq!(js_safe(u64::MAX), JS_MAX_SAFE_INTEGER);
    }

    #[test]
    fn outputs_of_one_outpoint_form_one_row() {
        let rows = outpoint_rows(&[
            output("a:0", "1:1", 5),
            output("a:0", "2:1", 6),
            output("a:1", "1:1", 7),
        ])
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rune_ids, vec!["1:1", "2:1"]);
        assert_eq!(rows[0].balances, vec![5, 6]);
        assert_eq!(rows[1].outpoint, "a:1");
    }

    fn spend(txid: &str, outpoints: &[&str]) -> opi_core::RuneSpend {
        opi_core::RuneSpend {
            txid: txid.into(),
            outpoints: outpoints.iter().map(|o| o.to_string()).collect(),
        }
    }

    #[test]
    fn inputs_lead_each_transaction_in_input_order() {
        let held = HashMap::from([
            (
                "a:0".to_string(),
                (vec![0x51], vec![("1:1".to_string(), 3)]),
            ),
            (
                "b:0".to_string(),
                (vec![0x52], vec![("1:1".to_string(), 4)]),
            ),
        ]);
        let events = complete_events(
            &[output("t:0", "1:1", 7)],
            &[spend("t", &["b:0", "z:9", "a:0"])],
            &held,
        )
        .unwrap();
        let inputs: Vec<_> = events
            .iter()
            .map(|e| (e.kind, e.outpoint.clone().unwrap(), e.amount))
            .collect();
        assert_eq!(
            inputs,
            [
                (RuneEventKind::Input, "b:0".to_string(), 4),
                (RuneEventKind::Input, "a:0".to_string(), 3),
                (RuneEventKind::Output, "t:0".to_string(), 7),
            ]
        );
        assert!(
            complete_events(&[output("t:0", "1:1", 8)], &[spend("t", &["a:0"])], &held).is_err()
        );
    }

    /// An etching with nothing to allocate has no events; its spend is fine
    /// while it spends no rune outpoint.
    #[test]
    fn a_spend_without_events_holds_no_runes() {
        let held = HashMap::from([(
            "a:0".to_string(),
            (vec![0x51], vec![("1:1".to_string(), 3)]),
        )]);
        assert!(
            complete_events(&[], &[spend("e", &["z:0"])], &held)
                .unwrap()
                .is_empty()
        );
        assert!(complete_events(&[], &[spend("e", &["a:0"])], &held).is_err());
    }

    #[test]
    fn counters_add_the_block_to_the_totals_in_rune_id_order() {
        let mint = |rune: &str| RuneEvent {
            kind: RuneEventKind::Mint,
            outpoint: None,
            pkscript: None,
            ..output("x", rune, 5)
        };
        let burn = |rune: &str, amount| RuneEvent {
            kind: RuneEventKind::Burn,
            outpoint: None,
            pkscript: None,
            ..output("x", rune, amount)
        };
        let before = HashMap::from([("2:1".to_string(), (10u128, 4u128))]);
        let counters = counters(
            &[
                mint("10:1"),
                burn("2:1", 6),
                mint("2:1"),
                mint("2:1"),
                output("x:0", "9:9", 1),
            ],
            &before,
        )
        .unwrap();
        let rows: Vec<_> = counters
            .iter()
            .map(|c| (c.rune_id.as_str(), c.burned, c.mints))
            .collect();
        assert_eq!(rows, [("2:1", 16, 6), ("10:1", 0, 1)]);
    }

    #[test]
    fn inputs_of_one_outpoint_spend_it_once() {
        let input = |outpoint: &str| RuneEvent {
            kind: RuneEventKind::Input,
            ..output(outpoint, "1:1", 1)
        };
        assert_eq!(
            spent_outpoints(&[input("a:0"), input("a:0"), input("b:0")]).unwrap(),
            vec!["a:0", "b:0"]
        );
    }
}
