mod support;

use {
    bitcoin::OutPoint,
    opi_core::BlockSource,
    opi_source::journal::{Journal, cf, info_key, table},
    ord::InscriptionId,
    redb::{ReadableDatabase, ReadableTableMetadata},
    support::{Harness, start, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"transfer","tick":"ordi","amt":"10"}"#;

fn outpoint(id: InscriptionId) -> OutPoint {
    OutPoint {
        txid: id.txid,
        vout: 0,
    }
}

fn has_info(journal: &Journal, id: InscriptionId) -> bool {
    let tx = journal.db().begin_read().unwrap();
    let infos = tx.open_table(table(cf::INFO)).unwrap();
    infos
        .get(info_key(&id.to_string()).unwrap().as_slice())
        .unwrap()
        .is_some()
}

fn rows_below(journal: &Journal, name: &'static str, height: u32) -> u64 {
    let tx = journal.db().begin_read().unwrap();
    let rows = tx.open_table(table(name)).unwrap();
    let end = height.to_be_bytes();
    rows.range::<&[u8]>(..end.as_slice()).unwrap().count() as u64
}

#[test]
fn prune_keeps_what_later_blocks_need() {
    let harness = Harness::new();
    let moved = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let kept = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    harness.transfer(outpoint(moved), 0);
    harness.mine(1);
    let moved_at = harness.height();
    harness.mine(2);

    let tmp = tempfile::tempdir().unwrap();
    let source = start(&harness, tmp.path());
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    let journal = reader.journal().clone();
    assert!(rows_below(&journal, cf::RAW_HEIGHTS, moved_at + 1) > 0);
    assert_eq!(reader.inscription_records(moved_at).unwrap().len(), 1);

    let floor = moved_at + 1;
    let pruned = journal.prune(floor, floor).unwrap();
    assert_eq!(pruned.transfers, 3, "{pruned:?}");
    assert_eq!(pruned.infos, 1, "{pruned:?}");
    assert!(pruned.raw_events > 0, "{pruned:?}");
    assert_eq!(journal.pruned_below().unwrap(), floor);
    assert_eq!(rows_below(&journal, cf::RAW_EVENTS, floor), 0);
    assert_eq!(rows_below(&journal, cf::RAW_HEIGHTS, floor), 0);
    assert_eq!(rows_below(&journal, cf::TRANSFERS, floor), 0);
    assert!(!has_info(&journal, moved));
    assert!(has_info(&journal, kept));

    let err = reader.block_view(moved_at).unwrap_err();
    assert!(format!("{err:#}").contains("pruned floor"), "{err:#}");
    assert!(reader.inscription_records(moved_at).is_err());
    assert!(reader.block_view(floor).unwrap().is_some());
    assert!(reader.block_info(moved_at).unwrap().is_some());

    // A second prune at the same bounds finds nothing.
    assert!(journal.prune(floor, floor).unwrap().is_empty());
    assert!(journal.prune(harness.height() + 2, floor).is_err());

    // The kept inscription's first transfer after the prune is still
    // recorded with its content.
    harness.transfer(outpoint(kept), 0);
    harness.mine(1);
    wait_for_tip(&reader, harness.height());
    let view = reader.block_view(harness.height()).unwrap().unwrap();
    assert_eq!(view.inscriptions.records.len(), 1);
    assert_eq!(
        view.inscriptions.records[0].inscription_id,
        kept.to_string()
    );
    assert_eq!(view.inscriptions.infos[&kept.to_string()].content, JSON);

    source.shutdown();
    source.join().unwrap();
    drop((reader, journal));

    let path = tmp.path().join("journal").join("journal.redb");
    Journal::compact(&path).unwrap();
    let journal = Journal::open(&path, ord::Chain::Regtest, true).unwrap();
    assert_eq!(journal.pruned_below().unwrap(), floor);
    let tx = journal.db().begin_read().unwrap();
    assert!(tx.open_table(table(cf::HEADERS)).unwrap().len().unwrap() > 0);
}
