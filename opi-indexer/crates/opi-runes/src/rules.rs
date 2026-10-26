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
