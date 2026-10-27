//! A source run without rune indexing: ord indexes no runes, the journal has
//! no rune tables and every block view carries an empty rune feed. An ord
//! index or journal built with the other setting is refused at startup.

mod support;

use {
    opi_core::{BlockSource, RuneFeed},
    opi_source::{
        driver::{SourceConfig, SourceHandle, spawn_source},
        journal::{JournalReader, cf},
        ord_settings,
    },
    ord::{Chain, Index},
    ordinals::{Etching, Runestone},
    redb::{ReadableDatabase, TableHandle},
    std::path::Path,
    support::{Harness, dump_tables, envelope, source_config, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"21000000"}"#;

fn config(harness: &Harness, dir: &Path, runes: bool) -> SourceConfig {
    SourceConfig {
        runes,
        ..source_config(dir, harness.core.url(), 2)
    }
}

fn synced(harness: &Harness, dir: &Path, runes: bool) -> (SourceHandle, JournalReader) {
    let source = spawn_source(config(harness, dir, runes)).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    (source, reader)
}

fn stop(source: SourceHandle) {
    source.shutdown();
    source.join().unwrap();
}

fn etching() -> Runestone {
    Runestone {
        etching: Some(Etching {
            premine: Some(1000),
            symbol: Some('$'),
            turbo: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn table_names(reader: &JournalReader) -> Vec<String> {
    reader
        .journal()
        .db()
        .begin_read()
        .unwrap()
        .list_tables()
        .unwrap()
        .map(|handle| handle.name().to_string())
        .collect()
}

/// The same chain synced with and without runes gives the same inscription
/// records, info rows, headers and first-transfer entries, and the same
/// block views apart from the rune feed.
#[test]
fn inscription_records_match_with_and_without_runes() {
    let harness = Harness::new();
    let funding = harness.fund();
    harness.runestone(&[funding], 1, etching());
    let first = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    harness.transfer(
        bitcoin::OutPoint {
            txid: first.txid,
            vout: 0,
        },
        100,
    );
    harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    harness.inscribe(
        b"{\"p\":\"sns\",\"op\":\"reg\",\"name\":\"a.sats\"}",
        "text/plain",
    );
    harness.mine(2);

    let tables = [cf::TRANSFERS, cf::INFO, cf::HEADERS, cf::FIRST_TRANSFER];
    let with_dir = tempfile::tempdir().unwrap();
    let without_dir = tempfile::tempdir().unwrap();
    let (with_source, with) = synced(&harness, with_dir.path(), true);
    let (without_source, without) = synced(&harness, without_dir.path(), false);

    let rows = dump_tables(with.journal().db(), &tables);
    for table in tables {
        assert!(
            rows.iter().any(|(name, _, _)| *name == table),
            "the chain leaves {table} empty"
        );
    }
    assert_eq!(dump_tables(without.journal().db(), &tables), rows);

    for height in 0..=harness.height() {
        let a = with.block_view(height).unwrap().unwrap();
        let b = without.block_view(height).unwrap().unwrap();
        assert_eq!(
            (&a.hash, a.time, &a.inscriptions),
            (&b.hash, b.time, &b.inscriptions),
            "height {height}"
        );
    }
    stop(with_source);
    stop(without_source);
}

#[test]
fn source_without_runes_finalizes_inscriptions_and_no_rune_records() {
    let harness = Harness::new();
    let funding = harness.fund();
    harness.runestone(&[funding], 1, etching());
    harness.mine(1);
    let rune_height = harness.height();
    let inscription = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let inscription_height = harness.height();

    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path(), false);
    assert!(!reader.journal().runes());

    for height in 0..=harness.height() {
        let view = reader.block_view(height).unwrap().unwrap();
        assert_eq!(view.runes, RuneFeed::default(), "height {height}");
    }
    assert!(
        reader
            .block_view(rune_height)
            .unwrap()
            .unwrap()
            .inscriptions
            .records
            .is_empty()
    );
    let view = reader.block_view(inscription_height).unwrap().unwrap();
    assert_eq!(view.inscriptions.records.len(), 1);
    assert_eq!(
        view.inscriptions.records[0].inscription_id,
        inscription.to_string()
    );
    assert!(
        !table_names(&reader)
            .iter()
            .any(|name| name == cf::RUNE_EVENTS)
    );
    stop(source);

    let settings = ord_settings(
        Chain::Regtest,
        &tmp.path().join("ord"),
        &harness.core.url(),
        None,
        false,
    )
    .unwrap();
    assert!(!Index::open(&settings).unwrap().has_rune_index());
}

#[test]
fn source_without_runes_rolls_back_a_reorg() {
    let harness = Harness::new();
    harness.mine(2);
    let a = harness.reveal(1, envelope(b"text/plain", JSON));
    harness.mine(1);
    let height = harness.height();

    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path(), false);
    assert_eq!(
        reader.inscription_records(height).unwrap()[0].inscription_id,
        a.to_string()
    );

    harness.invalidate_tip();
    let b = harness.reveal(2, envelope(b"text/plain", JSON));
    harness.mine(2);
    wait_for_tip(&reader, height + 1);

    assert_eq!(
        reader.block_info(height).unwrap().unwrap().hash,
        harness.core.state().hashes[height as usize].to_string()
    );
    let records = reader.inscription_records(height).unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].inscription_id, b.to_string());
    assert_eq!(reader.inscription_info(&a.to_string()).unwrap(), None);
    stop(source);
}

/// A journal built without runes is refused by a runes start, and one built
/// with runes by a start without them; the refused start leaves the journal
/// as it was.
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
        let journal_path = config.journal_path();
        let ord_data_dir = config.ord_data_dir.clone();
        let err = spawn_source(config).err().expect("startup refuses");
        let message = format!("{err:#}");
        let expected = if built_with {
            "built with rune indexing"
        } else {
            "built without rune indexing"
        };
        assert!(message.contains(expected), "{message}");
        assert!(
            message.contains(&journal_path.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&ord_data_dir.display().to_string()),
            "{message}"
        );
        assert!(journal_path.exists());

        let (source, reader) = synced(&harness, tmp.path(), built_with);
        assert_eq!(reader.journal().runes(), built_with);
        stop(source);
    }
}

/// An ord index built with one setting, under a new journal started with the
/// other, is refused with a message naming ord's data dir, and the new
/// journal is removed.
#[test]
fn startup_refuses_an_ord_index_built_with_the_other_setting() {
    let harness = Harness::new();
    harness.inscribe(JSON, "text/plain");
    harness.mine(1);

    for built_with in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let (source, _) = synced(&harness, tmp.path(), built_with);
        stop(source);

        let config = config(&harness, tmp.path(), !built_with);
        std::fs::remove_dir_all(&config.journal_dir).unwrap();
        let journal_path = config.journal_path();
        let ord_data_dir = config.ord_data_dir.clone();
        let err = spawn_source(config).err().expect("startup refuses");
        let message = format!("{err:#}");
        let expected = if built_with {
            "ord's index was built with rune indexing"
        } else {
            "ord's index was built without rune indexing"
        };
        assert!(message.contains(expected), "{message}");
        assert!(
            message.contains(&ord_data_dir.display().to_string()),
            "{message}"
        );
        assert!(!journal_path.exists(), "a refused start leaves no journal");
    }
}
