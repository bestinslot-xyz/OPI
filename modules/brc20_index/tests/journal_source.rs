use std::{collections::HashMap, sync::Arc};

use bitcoin::Network;
use brc20_index::client::{Brc20Source, JournalBrc20Source, get_wallet};
use opi_core::{BlockInfo, BlockSource, InscriptionInfo, InscriptionRecord};

const TXID: &str = "11223344556677889900aabbccddeeff00112233445566778899aabbccddeeff";
const P2TR: &str = "5120a37c3903c8d0db6512e2b40b0dffa05e5a3ab73603ce8c9c4b7771e5412328f9";
const P2WPKH: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
const P2PKH: &str = "76a914751e76e8199196d454941c45d1b3a323f1433bd688ac";
const OP_RETURN: &str = "6a0b68656c6c6f20776f726c64";

#[derive(Default)]
struct StubSource {
    tip: Option<u32>,
    blocks: HashMap<u32, BlockInfo>,
    records: HashMap<u32, Vec<InscriptionRecord>>,
    infos: HashMap<String, InscriptionInfo>,
}

impl BlockSource for StubSource {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Ok(self.tip)
    }

    fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(self.blocks.get(&height).cloned())
    }

    fn inscription_records(&self, height: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(self.records.get(&height).cloned().unwrap_or_default())
    }

    fn inscription_info(&self, inscription_id: &str) -> anyhow::Result<Option<InscriptionInfo>> {
        Ok(self.infos.get(inscription_id).cloned())
    }
}

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

fn journal(stub: StubSource) -> JournalBrc20Source {
    JournalBrc20Source::new(Arc::new(stub), Network::Bitcoin)
}

#[tokio::test]
async fn journal_source_matches_db_reader_shape() {
    let content = r#"{"p":"brc-20","op":"mint","tick":"ordi","amt":"1000"}"#;
    let mut i = info(0, content);
    i.parent_id = Some(inscription_id(5));
    let mut rec = record(840_000, 3, 0, P2TR);
    rec.old_satpoint = Some(format!("{TXID}:1:2"));
    rec.sent_as_fee = true;

    let mut stub = StubSource::default();
    stub.records.insert(840_000, vec![rec]);
    stub.infos.insert(inscription_id(0), i);

    let txes = journal(stub).get_transfers(840_000).await.unwrap();
    assert_eq!(txes.len(), 1);
    let tx = &txes[0];
    assert_eq!(tx.tx_id, "840000:3");
    assert_eq!(tx.inscription_id, inscription_id(0));
    assert_eq!(tx.inscription_number, 77);
    assert_eq!(tx.old_satpoint.as_deref(), Some(format!("{TXID}:1:2").as_str()));
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

#[tokio::test]
async fn cursed_and_non_json_are_filtered() {
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

    let mut stub = StubSource::default();
    let mut recs = Vec::new();
    for (idx, i) in [cursed, not_json, bad_type, bad_content, other_protocol, valid]
        .into_iter()
        .enumerate()
    {
        let index: u32 = i.inscription_id.rsplit('i').next().unwrap().parse().unwrap();
        recs.push(record(10, idx as u32, index, P2WPKH));
        stub.infos.insert(i.inscription_id.clone(), i);
    }
    // A second movement of an already rejected inscription is skipped too.
    recs.push(record(10, 6, 1, P2WPKH));
    stub.records.insert(10, recs);

    let txes = journal(stub).get_transfers(10).await.unwrap();
    assert_eq!(txes.len(), 1);
    assert_eq!(txes[0].tx_id, "10:5");
    assert_eq!(txes[0].inscription_id, inscription_id(6));
}

#[tokio::test]
async fn brc20_module_requires_module_field() {
    let mut stub = StubSource::default();
    let contents = [
        r#"{"p":"brc20-module","op":"deploy"}"#,
        r#"{"p":"brc20-module","module":"OTHER"}"#,
        r#"{"p":"brc20-module","module":"BRC20PROG"}"#,
    ];
    let mut recs = Vec::new();
    for (n, content) in contents.iter().enumerate() {
        let i = info(n as u32, content);
        stub.infos.insert(i.inscription_id.clone(), i);
        recs.push(record(5, n as u32, n as u32, P2PKH));
    }
    stub.records.insert(5, recs);

    let txes = journal(stub).get_transfers(5).await.unwrap();
    assert_eq!(txes.len(), 1);
    assert_eq!(txes[0].tx_id, "5:2");
}

#[test]
fn get_wallet_matches_db_reader() {
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

#[tokio::test]
async fn heights_and_hashes_come_from_the_source() {
    let mut stub = StubSource::default();
    stub.tip = Some(840_001);
    stub.blocks.insert(
        840_000,
        BlockInfo {
            height: 840_000,
            hash: "ab".repeat(32),
            time: 1_713_571_767,
        },
    );
    let source = journal(stub);
    assert_eq!(source.get_current_block_height().await.unwrap(), 840_001);
    assert_eq!(source.get_block_hash(840_000).await.unwrap(), "ab".repeat(32));
    assert_eq!(
        source.get_block_hash_and_time(840_000).await.unwrap(),
        ("ab".repeat(32), 1_713_571_767)
    );
    assert!(source.get_block_hash(1).await.is_err());
    assert!(source.get_block_hash_and_time(1).await.is_err());
    assert!(journal(StubSource::default()).get_current_block_height().await.is_err());
}
