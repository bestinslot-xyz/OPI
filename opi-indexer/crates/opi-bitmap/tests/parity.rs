use std::collections::BTreeMap;

use opi_bitmap::{
    bitmap_number, block_event_hash, candidates, cumulative_hash, event_str, is_valid_bitmap,
    sha256_hex,
};
use opi_core::{BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vector {
    content_hex: String,
    expected: Option<String>,
}

#[test]
fn bitmap_number_matches_python_vectors() {
    let vectors: Vec<Vector> =
        serde_json::from_str(include_str!("vectors.json")).expect("vectors.json");
    assert!(!vectors.is_empty());
    for vector in vectors {
        let content = hex::decode(&vector.content_hex).unwrap();
        // Python's integers are unbounded; numbers beyond u64 map to None.
        let expected = vector.expected.and_then(|n| n.parse::<u64>().ok());
        assert_eq!(bitmap_number(&content), expected, "{}", vector.content_hex);
    }
}

fn info(id: &str, number: i32, content: &str) -> InscriptionInfo {
    InscriptionInfo {
        inscription_id: id.into(),
        inscription_number: number,
        cursed_for_brc20: false,
        parent_id: None,
        is_json: false,
        content: content.as_bytes().to_vec(),
        content_type: b"text/plain;charset=utf-8".to_vec(),
        metaprotocol: Vec::new(),
    }
}

fn record(idx: u32, id: &str, old: Option<&str>) -> InscriptionRecord {
    InscriptionRecord {
        height: 0,
        idx,
        inscription_id: id.into(),
        old_satpoint: old.map(String::from),
        new_satpoint: "tx:0:0".into(),
        sent_as_fee: false,
        new_output_value: 546,
        txid: "tx".into(),
        new_pkscript: Vec::new(),
    }
}

fn block(height: u32, entries: Vec<(InscriptionInfo, Option<&str>)>) -> BlockView {
    let mut records = Vec::new();
    let mut infos = BTreeMap::new();
    for (idx, (info, old)) in entries.into_iter().enumerate() {
        records.push(record(idx as u32, &info.inscription_id, old));
        infos.insert(info.inscription_id.clone(), info);
    }
    BlockView {
        height,
        hash: "00".repeat(32),
        time: 0,
        inscriptions: InscriptionFeed { records, infos },
    }
}

#[test]
fn event_hash_of_empty_block_is_sha256_of_empty_string() {
    assert_eq!(
        block_event_hash(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn event_strings_join_with_pipe() {
    let events = vec![event_str(5, "aai0"), event_str(7, "bbi1")];
    assert_eq!(events[0], "inscribe;aai0;5");
    assert_eq!(
        block_event_hash(&events),
        sha256_hex("inscribe;aai0;5|inscribe;bbi1;7")
    );
}

#[test]
fn duplicate_bitmap_in_block_emits_one_event() {
    let view = block(
        900,
        vec![
            (info("bi0", 20, "5.bitmap"), None),
            (info("ai0", 10, "5.bitmap"), None),
        ],
    );
    let found = candidates(&view);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].inscription_id, "ai0");
    assert_eq!(found[0].inscription_number, 10);
}

#[test]
fn number_above_block_height_is_skipped() {
    let view = block(
        100,
        vec![
            (info("ai0", 1, "101.bitmap"), None),
            (info("bi0", 2, "100.bitmap"), None),
        ],
    );
    let found = candidates(&view);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].bitmap_number, 100);
}

#[test]
fn candidates_are_creations_in_ascending_number_order() {
    let view = block(
        900,
        vec![
            (info("ci0", 30, "3.bitmap"), None),
            (info("ai0", 10, "1.bitmap"), None),
            (info("bi0", 20, "2.bitmap"), Some("tx:1:0")),
            (info("di0", -4, "4.bitmap"), None),
        ],
    );
    let found: Vec<_> = candidates(&view)
        .into_iter()
        .map(|c| c.bitmap_number)
        .collect();
    assert_eq!(found, vec![1, 3]);
}

#[test]
fn validity_follows_the_bitmap_rules() {
    let mut json = info("a", 1, "1.bitmap");
    json.is_json = true;
    let mut html = info("a", 1, "1.bitmap");
    html.content_type = b"text/html".to_vec();
    assert!(is_valid_bitmap(&info("a", 0, "1.bitmap")));
    assert!(!is_valid_bitmap(&info("a", -1, "1.bitmap")));
    assert!(!is_valid_bitmap(&json));
    assert!(!is_valid_bitmap(&html));
}

#[test]
fn cumulative_hash_chains_from_previous() {
    let first = sha256_hex("x");
    assert_eq!(cumulative_hash(None, &first), first);
    let second = sha256_hex("y");
    assert_eq!(
        cumulative_hash(Some(&first), &second),
        sha256_hex(&format!("{first}{second}"))
    );
}
