use std::collections::BTreeMap;

use bitcoin::Network;
use opi_brc20::{
    config::Brc20IndexerConfig,
    database::get_brc20_database,
    module::{get_wallet, install_fresh_database, transfers},
};
use opi_core::{InscriptionFeed, InscriptionInfo, InscriptionRecord};

const TXID: &str = "11223344556677889900aabbccddeeff00112233445566778899aabbccddeeff";
const P2TR: &str = "5120a37c3903c8d0db6512e2b40b0dffa05e5a3ab73603ce8c9c4b7771e5412328f9";
const P2WPKH: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
const P2PKH: &str = "76a914751e76e8199196d454941c45d1b3a323f1433bd688ac";
const OP_RETURN: &str = "6a0b68656c6c6f20776f726c64";

fn inscription_id(index: u32) -> String {
    format!("{TXID}i{index}")
}

fn info(index: u32, content: &str) -> InscriptionInfo {
    InscriptionInfo {
        inscription_id: inscription_id(index),
        inscription_number: 77,
        cursed_for_brc20: false,
        parent_id: None,
        is_json: true,
        content: content.as_bytes().to_vec(),
        content_type: b"text/plain;charset=utf-8".to_vec(),
        metaprotocol: Vec::new(),
    }
}

fn record(height: u32, idx: u32, index: u32, pkscript: &str) -> InscriptionRecord {
    InscriptionRecord {
        height,
        idx,
        inscription_id: inscription_id(index),
        old_satpoint: None,
        new_satpoint: format!("{TXID}:0:0"),
        sent_as_fee: false,
        new_output_value: 546,
        txid: TXID.to_string(),
        new_pkscript: hex::decode(pkscript).unwrap(),
    }
}

fn feed(records: Vec<InscriptionRecord>, infos: Vec<InscriptionInfo>) -> InscriptionFeed {
    InscriptionFeed {
        records,
        infos: infos
            .into_iter()
            .map(|info| (info.inscription_id.clone(), info))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn apply_produces_same_transfers_as_journal_source() {
    let content = r#"{"p":"brc-20","op":"mint","tick":"ordi","amt":"1000"}"#;
    let mut i = info(0, content);
    i.parent_id = Some(inscription_id(5));
    let mut rec = record(840_000, 3, 0, P2TR);
    rec.old_satpoint = Some(format!("{TXID}:1:2"));
    rec.sent_as_fee = true;

    let txes = transfers(&feed(vec![rec], vec![i]), Network::Bitcoin).unwrap();
    assert_eq!(txes.len(), 1);
    let tx = &txes[0];
    assert_eq!(tx.tx_id, "840000:3");
    assert_eq!(tx.inscription_id, inscription_id(0));
    assert_eq!(tx.inscription_number, 77);
    assert_eq!(
        tx.old_satpoint.as_deref(),
        Some(format!("{TXID}:1:2").as_str())
    );
    assert_eq!(tx.new_satpoint, format!("{TXID}:0:0"));
    assert_eq!(tx.txid, TXID);
    assert_eq!(tx.new_pkscript, P2TR);
    assert_eq!(tx.new_wallet, get_wallet(P2TR, Network::Bitcoin));
    assert!(tx.new_wallet.starts_with("bc1p"));
    assert!(tx.sent_as_fee);
    assert_eq!(
        tx.content,
        serde_json::json!({"p":"brc-20","op":"mint","tick":"ordi","amt":"1000"})
    );
    assert_eq!(tx.byte_len, content.len() as u32);
    assert_eq!(tx.parent_id.as_deref(), Some(inscription_id(5).as_str()));
}

#[test]
fn cursed_and_non_json_are_filtered() {
    let content = r#"{"p":"brc-20","op":"mint","tick":"ordi","amt":"1"}"#;
    let mut cursed = info(1, content);
    cursed.cursed_for_brc20 = true;
    let mut not_json = info(2, content);
    not_json.is_json = false;
    let mut bad_type = info(3, content);
    bad_type.content_type = b"image/png".to_vec();
    let bad_content = info(4, "not json");
    let other_protocol = info(5, r#"{"p":"sns","op":"reg"}"#);
    let valid = info(6, content);

    let mut recs = Vec::new();
    let mut infos = Vec::new();
    for (idx, i) in [
        cursed,
        not_json,
        bad_type,
        bad_content,
        other_protocol,
        valid,
    ]
    .into_iter()
    .enumerate()
    {
        let index: u32 = i
            .inscription_id
            .rsplit('i')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        recs.push(record(10, idx as u32, index, P2WPKH));
        infos.push(i);
    }
    // A second movement of an already rejected inscription is skipped too.
    recs.push(record(10, 6, 1, P2WPKH));

    let txes = transfers(&feed(recs, infos), Network::Bitcoin).unwrap();
    assert_eq!(txes.len(), 1);
    assert_eq!(txes[0].tx_id, "10:5");
    assert_eq!(txes[0].inscription_id, inscription_id(6));
}

#[test]
fn brc20_module_requires_module_field() {
    let contents = [
        r#"{"p":"brc20-module","op":"deploy"}"#,
        r#"{"p":"brc20-module","module":"OTHER"}"#,
        r#"{"p":"brc20-module","module":"BRC20PROG"}"#,
    ];
    let mut recs = Vec::new();
    let mut infos = Vec::new();
    for (n, content) in contents.iter().enumerate() {
        infos.push(info(n as u32, content));
        recs.push(record(5, n as u32, n as u32, P2PKH));
    }

    let txes = transfers(&feed(recs, infos), Network::Bitcoin).unwrap();
    assert_eq!(txes.len(), 1);
    assert_eq!(txes[0].tx_id, "5:2");
}

#[test]
fn a_record_without_its_info_is_an_error() {
    let err = transfers(
        &feed(vec![record(7, 0, 9, P2TR)], Vec::new()),
        Network::Bitcoin,
    )
    .unwrap_err();
    assert!(err.to_string().contains(&inscription_id(9)));
}

#[test]
fn get_wallet_encodes_known_addresses() {
    assert_eq!(
        get_wallet(P2TR, Network::Bitcoin),
        "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297"
    );
    assert_eq!(
        get_wallet(P2WPKH, Network::Bitcoin),
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
    );
    assert_eq!(
        get_wallet(P2PKH, Network::Bitcoin),
        "1BgGZ9tcN4rm9KBzDn7KprQz87SZ26SAMH"
    );
    assert_eq!(get_wallet(OP_RETURN, Network::Bitcoin), "");
    assert!(get_wallet(P2WPKH, Network::Signet).starts_with("tb1q"));
}

/// Every module instance installs a new database in the global slot, so it
/// starts from what Postgres holds with empty caches and no pending writes.
/// The database's pool connects lazily, so no Postgres is needed.
#[tokio::test]
async fn each_instance_starts_from_a_fresh_database() {
    let config = Brc20IndexerConfig::default();
    for _ in 0..3 {
        install_fresh_database(&config).await;
        let database = get_brc20_database();
        let mut database = database.lock().await;
        assert_eq!(database.block_event_strings.len(), 0);
        assert_eq!(database.event_inserts.len(), 0);
        assert_eq!(database.current_event_id, 0);
        database
            .block_event_strings
            .insert(840_000, "partial".into());
        database.current_event_id = 42;
    }
}

/// The destructuring lists every field without `..`, so it stops compiling
/// when a field is added, including a BRC2.0 on/off switch.
#[test]
fn config_has_no_prog_toggle() {
    let Brc20IndexerConfig {
        light_client_mode: _,
        save_logs: _,
        db_host: _,
        db_port: _,
        db_user: _,
        db_password: _,
        db_database: _,
        db_ssl: _,
        report_to_indexer: _,
        report_all_blocks: _,
        report_url: _,
        report_retries: _,
        report_name: _,
        network_type: _,
        network_type_string: _,
        first_inscription_height: _,
        first_brc20_height: _,
        first_brc20_prog_phase_one_height: _,
        first_brc20_prog_all_tickers_height: _,
        first_brc20_prog_prague_height: _,
        self_mint_activation_height: _,
        brc20_swap_refund_activation_height: _,
        brc20_swap_refund_address: _,
        brc20_swap_module_pkscript: _,
        brc20_prog_rpc_server_enabled: _,
        brc20_prog_db_path: _,
        bitcoin_rpc_cache_enabled: _,
        bitcoin_rpc_url: _,
        bitcoin_rpc_auth: _,
        height_limit: _,
    } = Brc20IndexerConfig::default();
}
