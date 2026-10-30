//! A source run that keeps every inscription move: every bound creation and
//! every transfer lands in the block view, not only the BRC-20 ones the
//! transfer records keep. A journal built with the other setting is refused
//! at startup.

mod support;

use {
    bitcoin::{OutPoint, ScriptBuf, WPubkeyHash, hashes::Hash},
    opi_core::{BlockSource, InscriptionMove},
    opi_source::{
        driver::{SourceConfig, SourceHandle, spawn_source},
        journal::{JournalOptions, JournalReader},
    },
    ord::InscriptionId,
    std::path::Path,
    support::{Harness, envelope, envelope_with, source_config, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"21000000"}"#;
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn config(harness: &Harness, dir: &Path, inscriptions: bool) -> SourceConfig {
    SourceConfig {
        journal: JournalOptions {
            inscriptions,
            ..JournalOptions::default()
        },
        ..source_config(dir, harness.core.url(), 2)
    }
}

fn synced(harness: &Harness, dir: &Path, inscriptions: bool) -> (SourceHandle, JournalReader) {
    let source = spawn_source(config(harness, dir, inscriptions)).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    (source, reader)
}

fn stop(source: SourceHandle) {
    source.shutdown();
    source.join().unwrap();
}

fn moves(reader: &JournalReader, height: u32) -> Vec<InscriptionMove> {
    reader
        .block_view(height)
        .unwrap()
        .unwrap()
        .inscription_moves
}

fn reveal_outpoint(id: InscriptionId) -> OutPoint {
    OutPoint {
        txid: id.txid,
        vout: 0,
    }
}

#[test]
fn every_creation_and_transfer_is_a_move() {
    let harness = Harness::new();
    harness.mine(3);
    let json = harness.reveal(1, envelope(b"text/plain;charset=utf-8", JSON));
    harness.mine(1);
    let created = harness.height();
    let png = harness.reveal(2, envelope(b"image/png", PNG));
    let unbound = harness.reveal(
        3,
        envelope_with(&[(&[1], b"text/plain"), (&[22], b"x")], JSON),
    );
    harness.mine(1);
    let image_height = harness.height();
    let script = ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]));
    let first = harness.transfer_to(reveal_outpoint(json), &script);
    harness.mine(1);
    let first_height = harness.height();
    let second = harness.transfer_to(
        OutPoint {
            txid: first,
            vout: 0,
        },
        &script,
    );
    harness.mine(1);
    let second_height = harness.height();

    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path(), true);
    assert!(reader.journal().inscriptions());

    let at_creation = moves(&reader, created);
    assert_eq!(at_creation.len(), 1, "{at_creation:?}");
    let genesis = at_creation[0].genesis.as_ref().unwrap();
    assert_eq!(at_creation[0].inscription_id, json.to_string());
    assert_eq!(at_creation[0].satpoint, format!("{}:0:0", json.txid));
    assert_eq!(
        genesis.content_type.as_deref(),
        Some(b"text/plain;charset=utf-8".as_slice())
    );
    assert_eq!(genesis.number, 0);
    assert!(!at_creation[0].pkscript.is_empty());

    // The image is a move although the transfer records leave it out; the
    // unbound inscription has no location and is no move.
    let images = moves(&reader, image_height);
    assert_eq!(images.len(), 1, "{images:?}");
    assert_eq!(images[0].inscription_id, png.to_string());
    assert_eq!(
        images[0].genesis.as_ref().unwrap().content_type.as_deref(),
        Some(b"image/png".as_slice())
    );
    assert!(
        images
            .iter()
            .all(|m| m.inscription_id != unbound.to_string())
    );
    assert_eq!(
        images[0].sequence_number,
        at_creation[0].sequence_number + 1
    );

    let first_moves = moves(&reader, first_height);
    assert_eq!(first_moves.len(), 1, "{first_moves:?}");
    assert_eq!(first_moves[0].genesis, None);
    assert_eq!(first_moves[0].satpoint, format!("{first}:0:0"));
    assert_eq!(first_moves[0].pkscript, script.to_bytes());
    assert_eq!(
        first_moves[0].sequence_number,
        at_creation[0].sequence_number
    );

    // The second transfer has no transfer record, but it is a move.
    assert_eq!(
        reader.inscription_records(second_height).unwrap(),
        Vec::new()
    );
    let second_moves = moves(&reader, second_height);
    assert_eq!(second_moves.len(), 1, "{second_moves:?}");
    assert_eq!(second_moves[0].satpoint, format!("{second}:0:0"));
    stop(source);
}

#[test]
fn a_reorg_drops_the_replaced_moves() {
    let harness = Harness::new();
    harness.mine(2);
    let a = harness.reveal(1, envelope(b"image/png", PNG));
    harness.mine(1);
    let height = harness.height();

    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path(), true);
    assert_eq!(moves(&reader, height)[0].inscription_id, a.to_string());

    harness.invalidate_tip();
    let b = harness.reveal(2, envelope(b"image/png", PNG));
    harness.mine(2);
    wait_for_tip(&reader, height + 1);

    let replaced = moves(&reader, height);
    assert_eq!(replaced.len(), 1, "{replaced:?}");
    assert_eq!(replaced[0].inscription_id, b.to_string());
    assert!(moves(&reader, height + 1).is_empty());
    stop(source);
}

#[test]
fn a_journal_without_moves_has_none_in_its_views() {
    let harness = Harness::new();
    harness.inscribe(PNG, "image/png");
    harness.mine(1);

    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path(), false);
    assert!(!reader.journal().inscriptions());
    assert!(moves(&reader, harness.height()).is_empty());
    stop(source);
}

/// A journal built without moves is refused by a start that keeps them, and
/// one built with them by a start that does not; the refused start leaves the
/// journal as it was.
#[test]
fn startup_refuses_a_journal_built_with_the_other_setting() {
    let harness = Harness::new();
    harness.inscribe(JSON, "text/plain");
    harness.mine(1);

    for built_with in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let (source, _) = synced(&harness, tmp.path(), built_with);
        stop(source);

        let config = config(&harness, tmp.path(), !built_with);
        let journal_path = config.journal_dir.clone();
        let err = spawn_source(config).err().expect("startup refuses");
        let message = format!("{err:#}");
        let expected = if built_with {
            "built with the inscription moves"
        } else {
            "built without the inscription moves"
        };
        assert!(message.contains(expected), "{message}");
        assert!(
            message.contains(&journal_path.display().to_string()),
            "{message}"
        );

        let (source, reader) = synced(&harness, tmp.path(), built_with);
        assert_eq!(reader.journal().inscriptions(), built_with);
        stop(source);
    }
}
