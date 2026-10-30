use {
    opi_core::{
        InscriptionGenesis, InscriptionInfo, InscriptionMove, InscriptionRecord, RuneEtching,
        RuneEvent, RuneEventKind, RuneSpend, RuneTerms,
    },
    opi_source::journal::{
        DEFAULT_SEGMENT, Journal, JournalOptions, LEGACY_FILE, META_FILE, begin_write, cf,
        compress_info, decode_info, decode_inscription_move, decode_rune_etching,
        decode_rune_event, decode_rune_spend, decode_transfer, decompress_info, encode_info,
        encode_inscription_move, encode_rune_etching, encode_rune_event, encode_rune_spend,
        encode_transfer, inscription_move_key, rune_id_key, rune_id_string, rune_outpoint_key,
        table, tracked, transfer_key,
    },
    ord::Chain,
    redb::{ReadableDatabase, TableHandle},
    std::path::Path,
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
fn rune_spend_round_trips() {
    for spend in [
        RuneSpend {
            txid: TXID_A.to_owned(),
            outpoints: vec![format!("{TXID_B}:0"), format!("{TXID_C}:4294967295")],
        },
        RuneSpend {
            txid: TXID_C.to_owned(),
            outpoints: Vec::new(),
        },
    ] {
        let value = encode_rune_spend(&spend).unwrap();
        assert_eq!(&value[..32], reversed(&spend.txid).as_slice());
        assert_eq!(decode_rune_spend(&value).unwrap(), spend);
        assert!(decode_rune_spend(&value[..value.len() - 1]).is_err());
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
    let outpoint = rune_outpoint_key(&format!("{TXID_B}:7")).unwrap();
    assert_eq!(&outpoint[..32], reversed(TXID_B).as_slice());
    assert_eq!(&outpoint[32..], 7u32.to_le_bytes());
}

/// Writes `value` under `key` in the meta file of the closed journal in
/// `dir`.
fn put_meta(dir: &Path, key: &[u8], value: &[u8]) {
    let db = redb::Database::create(dir.join(META_FILE)).unwrap();
    let tx = begin_write(&db).unwrap();
    tx.open_table(table(cf::META))
        .unwrap()
        .insert(key, value)
        .unwrap();
    tx.commit().unwrap();
}

/// The value under `key` in the meta file of the closed journal in `dir`.
fn get_meta(dir: &Path, key: &[u8]) -> Option<Vec<u8>> {
    let db = redb::Database::create(dir.join(META_FILE)).unwrap();
    let tx = db.begin_read().unwrap();
    let table = tx.open_table(table(cf::META)).unwrap();
    table.get(key).unwrap().map(|v| v.value().to_vec())
}

#[test]
fn open_creates_meta_and_tracked_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert!(journal.created());
    assert_eq!(journal.final_tip().unwrap(), None);
    assert_eq!(journal.segment_starts(), Vec::<u32>::new());
    assert!(path.join(tracked::TRACKED_FILE).exists());
    drop(journal);
    assert_eq!(get_meta(&path, b"chain").unwrap(), b"regtest");
    assert_eq!(get_meta(&path, b"schema").unwrap(), 2u32.to_be_bytes());
    assert_eq!(
        get_meta(&path, b"segment").unwrap(),
        DEFAULT_SEGMENT.to_be_bytes()
    );
    assert!(
        !Journal::open(&path, Chain::Regtest, true)
            .unwrap()
            .created()
    );
}

/// A segment file holds every table, named after its first height.
#[test]
fn segments_hold_every_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
    let segment = journal.segment_mut(824_731).unwrap();
    assert_eq!(journal.segment_starts(), [824_000]);
    assert!(path.join("seg-000824000.redb").exists());
    let tx = segment.begin_read().unwrap();
    let names = tx
        .list_tables()
        .unwrap()
        .map(|handle| handle.name().to_string())
        .collect::<Vec<_>>();
    for name in [
        cf::TRANSFERS,
        cf::INFO,
        cf::HEADERS,
        cf::RAW_EVENTS,
        cf::RAW_HEIGHTS,
        cf::META,
        cf::RUNE_EVENTS,
        cf::RUNE_ETCHINGS,
        cf::RUNE_SPENT,
        cf::INSCRIPTION_MOVES,
    ] {
        assert!(names.iter().any(|have| have == name), "{name}");
    }
    assert!(journal.segment(825_000).unwrap().is_none());
}

/// A segment the registry let go while a writer still holds it comes back
/// as the same handle: redb opens a file only once.
#[test]
fn a_held_segment_is_shared_after_the_registry_lets_it_go() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&dir.path().join("journal"), Chain::Regtest, false).unwrap();
    let held = journal.segment_mut(0).unwrap();
    for height in [1000, 2000, 3000, 4000] {
        journal.segment_mut(height).unwrap();
    }
    let again = journal.segment(10).unwrap().unwrap();
    assert!(std::sync::Arc::ptr_eq(&held, &again));
    drop((held, again));
    for height in [1000, 2000, 3000] {
        journal.segment(height).unwrap();
    }
    // Nothing holds it now, so it opens again.
    journal.segment(10).unwrap().unwrap();
}

#[test]
fn reopen_rejects_chain_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    let small = JournalOptions {
        cache_bytes: 64 << 20,
        ..JournalOptions::default()
    };
    let journal = Journal::open_with(&path, Chain::Regtest, true, &small).unwrap();
    journal.set_gate_limit(7).unwrap();
    assert_eq!(journal.gate_limit().unwrap(), Some(7));
    drop(journal);
    let err = Journal::open(&path, Chain::Signet, true).err().unwrap();
    assert!(err.to_string().contains("chain"), "{err}");
}

#[test]
fn reopen_rejects_schema_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    put_meta(&path, b"schema", &3u32.to_be_bytes());
    let err = Journal::open(&path, Chain::Regtest, true).err().unwrap();
    assert!(err.to_string().contains("schema"), "{err}");
}

/// The segment length is fixed when the journal is created.
#[test]
fn reopen_rejects_another_segment_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    drop(Journal::open(&path, Chain::Regtest, true).unwrap());
    let other = JournalOptions {
        segment: 2000,
        ..JournalOptions::default()
    };
    let err = Journal::open_with(&path, Chain::Regtest, true, &other)
        .err()
        .unwrap();
    let message = format!("{err:#}");
    assert!(message.contains("OPI_JOURNAL_SEGMENT"), "{message}");
    assert!(message.contains("1000"), "{message}");
}

/// A directory holding a single-file journal is refused, and left as it is.
#[test]
fn open_refuses_the_single_file_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(LEGACY_FILE), b"old").unwrap();
    let err = Journal::open(&path, Chain::Regtest, true).err().unwrap();
    let message = format!("{err:#}");
    assert!(message.contains("single-file journal"), "{message}");
    assert!(!path.join(META_FILE).exists());
    assert_eq!(std::fs::read(path.join(LEGACY_FILE)).unwrap(), b"old");
}

/// A journal records the runes setting it was created with.
#[test]
fn open_records_the_runes_setting() {
    let dir = tempfile::tempdir().unwrap();
    let without = dir.path().join("without");
    assert!(
        !Journal::open(&without, Chain::Regtest, false)
            .unwrap()
            .runes()
    );
    assert_eq!(get_meta(&without, b"runes").unwrap(), [0]);

    let with = dir.path().join("with");
    assert!(Journal::open(&with, Chain::Regtest, true).unwrap().runes());
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
    assert!(!Journal::open(&path, Chain::Regtest, false).unwrap().runes());
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

fn with_moves() -> JournalOptions {
    JournalOptions {
        inscriptions: true,
        ..JournalOptions::default()
    }
}

#[test]
fn inscription_move_round_trips() {
    let genesis = InscriptionGenesis {
        number: -7,
        charms: 0x0204,
        content_type: Some(b"text/plain;charset=utf-8".to_vec()),
    };
    let moves = [
        InscriptionMove {
            inscription_id: format!("{TXID_A}i2"),
            sequence_number: 99,
            genesis: Some(genesis.clone()),
            satpoint: format!("{TXID_B}:5:77"),
            pkscript: vec![0x51, 0x20, 0xaa],
        },
        InscriptionMove {
            genesis: Some(InscriptionGenesis {
                content_type: None,
                ..genesis
            }),
            pkscript: Vec::new(),
            ..moves_base()
        },
        moves_base(),
    ];
    for m in moves {
        let value = encode_inscription_move(&m).unwrap();
        assert_eq!(decode_inscription_move(&value).unwrap(), m);
    }
    let mut value = encode_inscription_move(&moves_base()).unwrap();
    value.push(0);
    assert!(decode_inscription_move(&value).is_err());
    assert_eq!(inscription_move_key(5, 2), transfer_key(5, 2));
}

fn moves_base() -> InscriptionMove {
    InscriptionMove {
        inscription_id: format!("{TXID_B}i0"),
        sequence_number: u32::MAX,
        genesis: None,
        satpoint: format!("{TXID_A}:0:{}", u64::MAX),
        pkscript: vec![0x00, 0x14],
    }
}

/// A journal records the inscriptions setting it was created with; an open
/// with the other setting is refused.
#[test]
fn open_records_the_inscriptions_setting() {
    let dir = tempfile::tempdir().unwrap();
    let without = dir.path().join("without");
    let journal = Journal::open(&without, Chain::Regtest, false).unwrap();
    assert!(!journal.inscriptions());
    drop(journal);
    assert_eq!(get_meta(&without, b"inscriptions").unwrap(), [0]);
    let err = Journal::open_with(&without, Chain::Regtest, false, &with_moves())
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("built without the inscription moves"),
        "{err:#}"
    );

    let with = dir.path().join("with");
    let journal = Journal::open_with(&with, Chain::Regtest, false, &with_moves()).unwrap();
    assert!(journal.inscriptions());
    drop(journal);
    assert_eq!(get_meta(&with, b"inscriptions").unwrap(), [1]);
    let err = Journal::open(&with, Chain::Regtest, false).err().unwrap();
    assert!(
        format!("{err:#}").contains("built with the inscription moves"),
        "{err:#}"
    );
}

/// The final tip is the highest one any segment records, read when the
/// journal opens.
#[test]
fn final_tip_is_the_highest_segment_tip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    {
        let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
        for (height, tip) in [(839_000, Some(839_999u32)), (840_000, Some(840_017))] {
            let segment = journal.segment_mut(height).unwrap();
            let tx = begin_write(&segment).unwrap();
            if let Some(tip) = tip {
                tx.open_table(table(cf::META))
                    .unwrap()
                    .insert(b"final_tip".as_slice(), tip.to_be_bytes().as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        // A segment above the tip holding only raw events.
        journal.segment_mut(841_000).unwrap();
    }
    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(840_017));
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
