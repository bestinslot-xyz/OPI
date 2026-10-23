use {
    opi_core::{InscriptionInfo, InscriptionRecord},
    opi_source::journal::{
        Journal, cf, decode_info, decode_transfer, encode_info, encode_transfer, transfer_key,
    },
    ord::Chain,
};

const TXID_A: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const TXID_B: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
const TXID_C: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

fn reversed(hex_str: &str) -> Vec<u8> {
    let mut bytes = hex::decode(hex_str).unwrap();
    bytes.reverse();
    bytes
}

fn record(old: Option<&str>) -> InscriptionRecord {
    InscriptionRecord {
        height: 840_000,
        idx: 3,
        inscription_id: format!("{TXID_A}i2"),
        old_satpoint: old.map(str::to_owned),
        new_satpoint: format!("{TXID_B}:5:77"),
        sent_as_fee: true,
        new_output_value: 1000,
        txid: TXID_C.to_owned(),
        new_pkscript: vec![0x51, 0x20, 0xaa],
    }
}

fn info() -> InscriptionInfo {
    InscriptionInfo {
        inscription_id: format!("{TXID_A}i0"),
        inscription_number: -7,
        cursed_for_brc20: true,
        parent_id: Some(format!("{TXID_B}i4")),
        is_json: true,
        content: b"{\"p\":\"brc-20\"}".to_vec(),
        content_type: b"text/plain".to_vec(),
        metaprotocol: b"brc-20".to_vec(),
    }
}

#[test]
fn transfer_key_is_height_then_idx_big_endian() {
    assert_eq!(
        transfer_key(0x01020304, 0x0a0b0c0d),
        [1, 2, 3, 4, 10, 11, 12, 13]
    );
}

#[test]
fn transfer_layout_matches_fork() {
    let r = record(None);
    let mut expected = Vec::new();
    expected.extend(reversed(TXID_A));
    expected.extend(2u32.to_be_bytes());
    expected.extend([0u8; 32]);
    expected.extend(u32::MAX.to_le_bytes());
    expected.extend(0u64.to_le_bytes());
    expected.extend(reversed(TXID_B));
    expected.extend(5u32.to_le_bytes());
    expected.extend(77u64.to_le_bytes());
    expected.push(0x01);
    expected.extend(1000u64.to_be_bytes());
    expected.extend(reversed(TXID_C));
    expected.extend([0x51, 0x20, 0xaa]);

    let bytes = encode_transfer(&r).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(&bytes[36..80].len(), &44);
}

#[test]
fn transfer_round_trips() {
    for old in [None, Some(format!("{TXID_C}:1:9"))] {
        let r = record(old.as_deref());
        let value = encode_transfer(&r).unwrap();
        let key = transfer_key(r.height, r.idx);
        assert_eq!(decode_transfer(&key, &value).unwrap(), r);
    }
}

#[test]
fn decode_transfer_reads_null_old_satpoint_as_none() {
    let r = record(None);
    let value = encode_transfer(&r).unwrap();
    let decoded = decode_transfer(&transfer_key(r.height, r.idx), &value).unwrap();
    assert_eq!(decoded.old_satpoint, None);
}

#[test]
fn decode_transfer_rejects_truncated_value() {
    let r = record(None);
    let value = encode_transfer(&r).unwrap();
    assert!(decode_transfer(&transfer_key(1, 1), &value[..100]).is_err());
}

#[test]
fn info_round_trips() {
    let i = info();
    let value = encode_info(&i).unwrap();
    assert_eq!(decode_info(&i.inscription_id, &value).unwrap(), i);

    let mut no_parent = info();
    no_parent.parent_id = None;
    no_parent.content.clear();
    let value = encode_info(&no_parent).unwrap();
    assert_eq!(
        decode_info(&no_parent.inscription_id, &value).unwrap(),
        no_parent
    );
}

#[test]
fn info_layout_matches_fork() {
    let mut i = info();
    i.parent_id = None;
    i.inscription_number = 5;
    i.cursed_for_brc20 = false;
    let mut expected = Vec::new();
    expected.extend(5i32.to_be_bytes());
    expected.push(0);
    expected.extend([0u8; 32]);
    expected.extend(0u32.to_be_bytes());
    expected.push(1);
    expected.extend(14u32.to_be_bytes());
    expected.extend(b"{\"p\":\"brc-20\"}");
    expected.extend(10u32.to_be_bytes());
    expected.extend(b"text/plain");
    expected.extend(6u32.to_be_bytes());
    expected.extend(b"brc-20");
    assert_eq!(encode_info(&i).unwrap(), expected);

    let with_parent = encode_info(&info()).unwrap();
    assert_eq!(&with_parent[5..37], reversed(TXID_B).as_slice());
    assert_eq!(&with_parent[37..41], 4u32.to_be_bytes());
}

#[test]
fn journal_open_writes_meta_and_rejects_other_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest).unwrap();
        assert_eq!(journal.final_tip().unwrap(), None);
        let meta = journal.db().cf_handle(cf::META).unwrap();
        assert_eq!(
            journal.db().get_cf(&meta, b"chain").unwrap().unwrap(),
            b"regtest"
        );
        assert_eq!(
            journal.db().get_cf(&meta, b"schema").unwrap().unwrap(),
            1u32.to_be_bytes()
        );
        for name in [
            cf::TRANSFERS,
            cf::INFO,
            cf::HEADERS,
            cf::RAW_EVENTS,
            cf::RAW_HEIGHTS,
        ] {
            assert!(journal.db().cf_handle(name).is_some(), "{name}");
        }
    }
    Journal::open(&path, Chain::Regtest).unwrap();
    let err = Journal::open(&path, Chain::Signet).err().unwrap();
    assert!(err.to_string().contains("chain"), "{err}");
}

#[test]
fn journal_final_tip_reads_meta() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest).unwrap();
        let meta = journal.db().cf_handle(cf::META).unwrap();
        journal
            .db()
            .put_cf(&meta, b"final_tip", 840_000u32.to_be_bytes())
            .unwrap();
        assert_eq!(journal.final_tip().unwrap(), Some(840_000));
    }
    let ro = Journal::open_read_only(&path).unwrap();
    assert_eq!(ro.final_tip().unwrap(), Some(840_000));
}
