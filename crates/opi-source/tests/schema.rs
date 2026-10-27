use {
    opi_core::{
        InscriptionInfo, InscriptionRecord, RuneEtching, RuneEvent, RuneEventKind, RuneTerms,
    },
    opi_source::journal::{
        Journal, cf, compress_info, decode_info, decode_rune_balance_row, decode_rune_counter_log,
        decode_rune_etching, decode_rune_event, decode_transfer, decompress_info, encode_info,
        encode_rune_balance_row, encode_rune_counter_log, encode_rune_etching, encode_rune_event,
        encode_transfer, rune_counter_log_key, rune_id_key, rune_id_string, rune_outpoint_key,
        table, transfer_key,
    },
    ord::Chain,
    redb::{ReadableDatabase, TableHandle},
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

const KINDS: [RuneEventKind; 5] = [
    RuneEventKind::Input,
    RuneEventKind::NewAllocation,
    RuneEventKind::Mint,
    RuneEventKind::Output,
    RuneEventKind::Burn,
];

#[test]
fn rune_event_round_trips() {
    for kind in KINDS {
        for (outpoint, pkscript) in [
            (None, None),
            (Some(format!("{TXID_B}:7")), None),
            (None, Some(vec![])),
            (
                Some(format!("{TXID_B}:4294967295")),
                Some(vec![0x51, 0x20, 0xaa]),
            ),
        ] {
            let event = RuneEvent {
                kind,
                txid: TXID_A.to_owned(),
                outpoint,
                pkscript,
                rune_id: "840000:12".to_owned(),
                amount: u128::MAX,
            };
            let value = encode_rune_event(&event).unwrap();
            assert_eq!(decode_rune_event(&value).unwrap(), event);
        }
    }
}

#[test]
fn rune_event_kind_ids_and_names() {
    let names = ["input", "new_rune_allocation", "mint", "output", "burn"];
    for (i, kind) in KINDS.into_iter().enumerate() {
        assert_eq!(kind.id(), i as i16);
        assert_eq!(kind.name(), names[i]);
    }
}

#[test]
fn rune_event_rejects_truncated_and_trailing() {
    let event = RuneEvent {
        kind: RuneEventKind::Output,
        txid: TXID_A.to_owned(),
        outpoint: Some(format!("{TXID_B}:1")),
        pkscript: Some(vec![1, 2, 3]),
        rune_id: "1:0".to_owned(),
        amount: 5,
    };
    let value = encode_rune_event(&event).unwrap();
    assert!(decode_rune_event(&value[..value.len() - 1]).is_err());
    let mut long = value.clone();
    long.push(0);
    assert!(decode_rune_event(&long).is_err());
    let mut bad_kind = value;
    bad_kind[0] = 9;
    assert!(decode_rune_event(&bad_kind).is_err());
}

fn etching(terms: Option<RuneTerms>, symbol: Option<char>) -> RuneEtching {
    RuneEtching {
        rune_id: "840001:3".to_owned(),
        block: 840_001,
        divisibility: 18,
        etching: TXID_C.to_owned(),
        terms,
        number: 99,
        premine: u128::MAX,
        rune: "UNCOMMONGOODS".to_owned(),
        spacers: 0b1000_0001,
        symbol,
        timestamp: 1_700_000_000,
        turbo: true,
    }
}

#[test]
fn rune_etching_round_trips() {
    let terms = [
        None,
        Some(RuneTerms {
            amount: None,
            cap: None,
            height: (None, None),
            offset: (None, None),
        }),
        Some(RuneTerms {
            amount: Some(u128::MAX),
            cap: Some(21_000_000),
            height: (Some(1), Some(u64::MAX)),
            offset: (Some(0), None),
        }),
    ];
    for terms in terms {
        for symbol in [None, Some('⧉')] {
            let e = etching(terms.clone(), symbol);
            let value = encode_rune_etching(&e).unwrap();
            assert_eq!(decode_rune_etching(&value).unwrap(), e);
        }
    }
    let mut unturbo = etching(None, None);
    unturbo.turbo = false;
    let value = encode_rune_etching(&unturbo).unwrap();
    assert_eq!(decode_rune_etching(&value).unwrap(), unturbo);
    assert!(decode_rune_etching(&value[..value.len() - 1]).is_err());
}

#[test]
fn rune_balance_row_round_trips() {
    let balances = vec![
        ("1:0".to_owned(), 5u128),
        ("840000:12".to_owned(), u128::MAX),
    ];
    let value = encode_rune_balance_row(&[0x51, 0x20, 0xaa], &balances).unwrap();
    let (pkscript, decoded) = decode_rune_balance_row(&value).unwrap();
    assert_eq!(pkscript, vec![0x51, 0x20, 0xaa]);
    assert_eq!(decoded, balances);

    let value = encode_rune_balance_row(&[], &[]).unwrap();
    assert_eq!(
        decode_rune_balance_row(&value).unwrap(),
        (Vec::new(), Vec::new())
    );
    assert!(decode_rune_balance_row(&value[..value.len() - 1]).is_err());
}

#[test]
fn rune_counter_log_round_trips() {
    for before in [None, Some((1u128, 2u128))] {
        let value = encode_rune_counter_log(before, (u128::MAX, 7));
        assert_eq!(
            decode_rune_counter_log(&value).unwrap(),
            (before, (u128::MAX, 7))
        );
    }
}

#[test]
fn rune_keys_sort_numerically() {
    assert_eq!(
        rune_id_key("1:0").unwrap(),
        [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]
    );
    assert!(rune_id_key("2:0").unwrap() > rune_id_key("1:9").unwrap());
    assert!(rune_id_key("1:10").unwrap() > rune_id_key("1:9").unwrap());
    assert!(rune_id_key("1:2:3").is_err());
    assert_eq!(
        rune_id_string(&rune_id_key("840000:12").unwrap()),
        "840000:12"
    );
    let key = rune_counter_log_key(5, "1:2").unwrap();
    assert_eq!(&key[..4], 5u32.to_be_bytes());
    assert_eq!(&key[4..], rune_id_key("1:2").unwrap());
    let outpoint = rune_outpoint_key(&format!("{TXID_B}:7")).unwrap();
    assert_eq!(&outpoint[..32], reversed(TXID_B).as_slice());
    assert_eq!(&outpoint[32..], 7u32.to_le_bytes());
}

#[test]
fn open_creates_rune_tables() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("journal"), Chain::Regtest, true).unwrap();
    let tx = journal.db().begin_read().unwrap();
    for name in [
        cf::RUNE_EVENTS,
        cf::RUNE_ETCHINGS,
        cf::RUNE_COUNTERS,
        cf::RUNE_COUNTER_LOG,
        cf::RUNE_BALANCES,
    ] {
        tx.open_table(table(name)).expect(name);
    }
}

fn put_meta(journal: &Journal, key: &[u8], value: &[u8]) {
    let mut tx = journal.db().begin_write().unwrap();
    tx.set_durability(redb::Durability::Immediate).unwrap();
    {
        let mut table = tx.open_table(table(cf::META)).unwrap();
        table.insert(key, value).unwrap();
    }
    tx.commit().unwrap();
}

fn get_meta(journal: &Journal, key: &[u8]) -> Option<Vec<u8>> {
    let tx = journal.db().begin_read().unwrap();
    let table = tx.open_table(table(cf::META)).unwrap();
    table.get(key).unwrap().map(|v| v.value().to_vec())
}

#[test]
fn open_creates_tables_and_meta() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert_eq!(journal.final_tip().unwrap(), None);
    assert_eq!(get_meta(&journal, b"chain").unwrap(), b"regtest");
    assert_eq!(get_meta(&journal, b"schema").unwrap(), 1u32.to_be_bytes());
    let tx = journal.db().begin_read().unwrap();
    for name in [
        cf::TRANSFERS,
        cf::INFO,
        cf::HEADERS,
        cf::RAW_EVENTS,
        cf::RAW_HEIGHTS,
        cf::META,
    ] {
        tx.open_table(table(name)).expect(name);
    }
}

#[test]
fn reopen_rejects_chain_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    let err = Journal::open(&path, Chain::Signet, true).err().unwrap();
    assert!(err.to_string().contains("chain"), "{err}");
}

#[test]
fn reopen_rejects_schema_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        put_meta(&journal, b"schema", &2u32.to_be_bytes());
    }
    let err = Journal::open(&path, Chain::Regtest, true).err().unwrap();
    assert!(err.to_string().contains("schema"), "{err}");
}

/// A journal with a finalized tip and without the rune tables is refused; one
/// with the rune tables opens.
#[test]
fn reopen_rejects_finalized_journal_without_rune_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        put_meta(&journal, b"final_tip", &10u32.to_be_bytes());
    }
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(10));
    {
        let mut tx = journal.db().begin_write().unwrap();
        tx.set_durability(redb::Durability::Immediate).unwrap();
        for name in [cf::RUNE_COUNTER_LOG, cf::RUNE_BALANCES] {
            assert!(tx.delete_table(table(name)).unwrap(), "{name}");
        }
        tx.commit().unwrap();
    }
    drop(journal);
    let err = Journal::open(&path, Chain::Regtest, true).err().unwrap();
    assert!(err.to_string().contains("predates rune indexing"), "{err}");
}

/// Without a finalized tip, a journal missing the rune tables gains them.
#[test]
fn reopen_adds_rune_tables_before_any_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        let mut tx = journal.db().begin_write().unwrap();
        tx.set_durability(redb::Durability::Immediate).unwrap();
        assert!(tx.delete_table(table(cf::RUNE_BALANCES)).unwrap());
        tx.commit().unwrap();
    }
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    let tx = journal.db().begin_read().unwrap();
    tx.open_table(table(cf::RUNE_BALANCES)).unwrap();
}

fn table_names(journal: &Journal) -> Vec<String> {
    journal
        .db()
        .begin_read()
        .unwrap()
        .list_tables()
        .unwrap()
        .map(|handle| handle.name().to_string())
        .collect()
}

fn remove_meta(journal: &Journal, key: &[u8]) {
    let mut tx = journal.db().begin_write().unwrap();
    tx.set_durability(redb::Durability::Immediate).unwrap();
    tx.open_table(table(cf::META)).unwrap().remove(key).unwrap();
    tx.commit().unwrap();
}

/// A journal created without runes records the setting and has no rune
/// tables; one created with runes records it and has them.
#[test]
fn open_records_the_runes_setting() {
    let dir = tempfile::tempdir().unwrap();
    let without = Journal::open(&dir.path().join("without"), Chain::Regtest, false).unwrap();
    assert!(!without.runes());
    assert_eq!(get_meta(&without, b"runes").unwrap(), [0]);
    let names = table_names(&without);
    for name in [
        cf::RUNE_EVENTS,
        cf::RUNE_ETCHINGS,
        cf::RUNE_COUNTERS,
        cf::RUNE_COUNTER_LOG,
        cf::RUNE_BALANCES,
    ] {
        assert!(!names.iter().any(|have| have == name), "{name}");
    }
    for name in [cf::TRANSFERS, cf::INFO, cf::HEADERS, cf::RAW_EVENTS] {
        assert!(names.iter().any(|have| have == name), "{name}");
    }

    let with = Journal::open(&dir.path().join("with"), Chain::Regtest, true).unwrap();
    assert!(with.runes());
    assert_eq!(get_meta(&with, b"runes").unwrap(), [1]);
}

#[test]
fn reopen_without_runes_keeps_the_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, false).unwrap());
    let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
    assert!(!journal.runes());
    assert!(!journal.created());
}

#[test]
fn reopen_with_runes_refuses_a_journal_built_without() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, false).unwrap());
    let err = Journal::open(&path, Chain::Regtest, true).err().unwrap();
    let message = format!("{err:#}");
    assert!(message.contains("built without rune indexing"), "{message}");
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("ord's data dir"), "{message}");
    // The refused open changes nothing.
    let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
    assert!(
        !table_names(&journal)
            .iter()
            .any(|have| have == cf::RUNE_EVENTS)
    );
}

#[test]
fn reopen_without_runes_refuses_a_journal_built_with() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    let err = Journal::open(&path, Chain::Regtest, false).err().unwrap();
    let message = format!("{err:#}");
    assert!(message.contains("built with rune indexing"), "{message}");
    assert!(message.contains(&path.display().to_string()), "{message}");
}

/// A finalized journal without the runes meta row counts as built with runes
/// when it has the rune tables, and gains the row on a runes open.
#[test]
fn journal_without_the_runes_row_follows_its_rune_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        put_meta(&journal, b"final_tip", &10u32.to_be_bytes());
        remove_meta(&journal, b"runes");
    }
    let err = Journal::open(&path, Chain::Regtest, false).err().unwrap();
    assert!(
        format!("{err:#}").contains("built with rune indexing"),
        "{err:#}"
    );
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert!(journal.runes());
    assert_eq!(get_meta(&journal, b"runes").unwrap(), [1]);
}

/// A journal without the runes meta row and without finalized blocks takes
/// the setting it is opened with.
#[test]
fn unfinalized_journal_without_the_runes_row_takes_the_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        remove_meta(&journal, b"runes");
    }
    let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
    assert!(!journal.runes());
    assert_eq!(get_meta(&journal, b"runes").unwrap(), [0]);
}

#[test]
fn final_tip_reads_meta() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    put_meta(&journal, b"final_tip", &840_000u32.to_be_bytes());
    assert_eq!(journal.final_tip().unwrap(), Some(840_000));
}

#[test]
fn info_compression_round_trips() {
    let body = format!(
        "{{\"p\":\"brc-20\",\"op\":\"mint\",\"tick\":\"ordi\",\"amt\":\"{}\"}}",
        "1".repeat(4_000)
    );
    let value = body.as_bytes()[..4096.min(body.len())].to_vec();
    let packed = compress_info(&value).unwrap();
    assert!(packed.len() < value.len() / 2, "{}", packed.len());
    assert_eq!(decompress_info(&packed).unwrap(), value);
}
