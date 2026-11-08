use opi_core::{RuneEvent, RuneEventKind};
use opi_runes::{
    RunesConfig, block_event_hash, cumulative_hash, event_str, hash_start_height, sha256_hex,
    start_height, symbol_hex, wallet_addr,
};

fn event(
    kind: RuneEventKind,
    outpoint: Option<&str>,
    pkscript: Option<&str>,
    rune_id: &str,
    amount: u128,
) -> RuneEvent {
    RuneEvent {
        kind,
        txid: "ab".repeat(32),
        outpoint: outpoint.map(str::to_string),
        pkscript: pkscript.map(|hex_str| hex::decode(hex_str).unwrap()),
        rune_id: rune_id.into(),
        amount,
    }
}

#[test]
fn event_str_formats_each_kind() {
    let outpoint = format!("{}:1", "cd".repeat(32));
    let pkscript = format!("0014{}", "11".repeat(20));
    assert_eq!(
        event_str(&event(
            RuneEventKind::Input,
            Some(&outpoint),
            Some(&pkscript),
            "840000:3",
            u128::MAX
        )),
        format!("input;{outpoint};{pkscript};840000:3;340282366920938463463374607431768211455")
    );
    assert_eq!(
        event_str(&event(
            RuneEventKind::NewAllocation,
            None,
            None,
            "840000:3",
            0
        )),
        "new_rune_allocation;;;840000:3;0"
    );
    assert_eq!(
        event_str(&event(RuneEventKind::Mint, None, None, "1:0", 1)),
        "mint;;;1:0;1"
    );
    assert_eq!(
        event_str(&event(
            RuneEventKind::Output,
            Some(&outpoint),
            Some(&pkscript),
            "2:5",
            42
        )),
        format!("output;{outpoint};{pkscript};2:5;42")
    );
    assert_eq!(
        event_str(&event(RuneEventKind::Burn, None, None, "2:5", 0)),
        "burn;;;2:5;0"
    );
}

#[test]
fn empty_block_hash_is_sha256_of_empty_string() {
    assert_eq!(
        block_event_hash(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn block_hash_joins_events_with_pipes() {
    let events = vec!["mint;;;1:0;1".to_string(), "burn;;;1:0;0".to_string()];
    assert_eq!(
        block_event_hash(&events),
        sha256_hex("mint;;;1:0;1|burn;;;1:0;0")
    );
}

#[test]
fn cumulative_hash_chains() {
    let first = block_event_hash(&["mint;;;1:0;1".to_string()]);
    assert_eq!(cumulative_hash(None, &first), first);
    let second = block_event_hash(&[]);
    assert_eq!(
        cumulative_hash(Some(&first), &second),
        sha256_hex(&format!("{first}{second}"))
    );
}

#[test]
fn symbol_hex_pads_utf8() {
    assert_eq!(symbol_hex('$'), "24000000");
    assert_eq!(symbol_hex('⧉'), "e2a78900");
    assert_eq!(symbol_hex('😀'), "f09f9880");
}

#[test]
fn wallet_addr_covers_the_five_types_and_networks() {
    let addr = |hex_str: &str, network: &str| wallet_addr(&hex::decode(hex_str).unwrap(), network);
    let p2pkh = format!("76a914{}88ac", "11".repeat(20));
    assert_eq!(
        addr(&p2pkh, "mainnet").as_deref(),
        Some("12ZEw5Hcv1hTb6YUQJ69y1V7uhcoDz92PH")
    );
    assert_eq!(
        addr(&p2pkh, "testnet").as_deref(),
        Some("mh5CE8Nbj38iND267s4XnvhSmhDW7yWc6Q")
    );
    assert_eq!(
        addr(&format!("a914{}87", "22".repeat(20)), "mainnet").as_deref(),
        Some("34oVnh4gNviJGMnNvgquMeLAxvXJuaRVMZ")
    );
    assert_eq!(
        addr(&format!("0014{}", "33".repeat(20)), "signet").as_deref(),
        Some("tb1qxvenxvenxvenxvenxvenxvenxvenxvenqzqps5")
    );
    assert_eq!(
        addr(&format!("0020{}", "44".repeat(32)), "regtest").as_deref(),
        Some("bcrt1qg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zqhkv0pq")
    );
    assert_eq!(
        addr(&format!("5120{}", "55".repeat(32)), "testnet4").as_deref(),
        Some("tb1p242424242424242424242424242424242424242424242424242std2dxg")
    );
    // Not a valid x-only key; the address is still encoded.
    assert_eq!(
        addr(
            "512057cd4cfa03f27f7b18c2fe45fe2c2e0f7b5ccb034af4dec098977c28562be7a2",
            "mainnet"
        )
        .as_deref(),
        Some("bc1p2lx5e7sr7flhkxxzlezlutpwpaa4ejcrft6dasycja7zs43tu73qqshdl2")
    );
    // A version 2 witness program, OP_RETURN and a v0 program of another
    // length get no address.
    assert_eq!(addr(&format!("5220{}", "66".repeat(32)), "mainnet"), None);
    assert_eq!(addr("6a0401020304", "mainnet"), None);
    assert_eq!(addr(&format!("0018{}", "77".repeat(24)), "mainnet"), None);
    assert_eq!(addr("", "mainnet"), None);
}

#[test]
fn heights_per_network() {
    assert_eq!(start_height("mainnet"), 840_000);
    assert_eq!(start_height("testnet"), 2_520_000);
    assert_eq!(start_height("testnet4"), 0);
    assert_eq!(start_height("signet"), 0);
    assert_eq!(start_height("regtest"), 0);
    assert_eq!(hash_start_height("mainnet"), 840_000);
    assert_eq!(hash_start_height("testnet"), 2_520_000);
    assert_eq!(hash_start_height("testnet4"), 0);
    assert_eq!(hash_start_height("signet"), 173_831);
    assert_eq!(hash_start_height("regtest"), 0);
}

#[test]
fn config_requires_a_database_and_a_known_network() {
    let lookup = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    };
    let config = RunesConfig::from_lookup(&lookup(&[
        ("RUNES_DB_DATABASE", "runes_db"),
        ("NETWORK_TYPE", "signet"),
        ("RUNES_DB_SSL", "true"),
    ]))
    .unwrap();
    assert_eq!(config.db_database, "runes_db");
    assert_eq!(config.network_type, "signet");
    assert!(config.db_ssl);
    assert_eq!(config.db_user, "postgres");
    assert_eq!(config.report_name, "opi_runes_indexer");
    assert!(RunesConfig::from_lookup(&lookup(&[("NETWORK_TYPE", "signet")])).is_err());
    assert!(
        RunesConfig::from_lookup(&lookup(&[
            ("RUNES_DB_DATABASE", "runes_db"),
            ("NETWORK_TYPE", "liquid"),
        ]))
        .is_err()
    );
}
