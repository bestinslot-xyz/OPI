mod support;

use {
    opi_core::{BlockSource, BlockView, InscriptionFeed},
    opi_source::journal::JournalReader,
    support::{Harness, start, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"21000000"}"#;

/// Runs a source over the harness chain and returns a reader at its tip.
fn synced(
    harness: &Harness,
    dir: &std::path::Path,
) -> (opi_source::driver::SourceHandle, JournalReader) {
    let source = start(harness, dir);
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    (source, reader)
}

fn stop(source: opi_source::driver::SourceHandle) {
    source.shutdown();
    source.join().unwrap();
}

#[test]
fn block_view_matches_individual_reads() {
    let harness = Harness::new();
    harness.inscribe(JSON, "text/plain");
    harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let mut checked = 0;
    for height in 0..=harness.height() {
        let view = reader.block_view(height).unwrap().unwrap();
        let block = reader.block_info(height).unwrap().unwrap();
        let records = reader.inscription_records(height).unwrap();
        let infos = records
            .iter()
            .map(|record| {
                (
                    record.inscription_id.clone(),
                    reader
                        .inscription_info(&record.inscription_id)
                        .unwrap()
                        .unwrap(),
                )
            })
            .collect();
        checked += records.len();
        assert_eq!(
            view,
            BlockView {
                height: block.height,
                hash: block.hash,
                time: block.time,
                inscriptions: InscriptionFeed { records, infos },
            }
        );
    }
    assert!(checked >= 2);
    stop(source);
}

#[test]
fn block_view_of_quiet_height_has_no_records() {
    let harness = Harness::new();
    harness.inscribe(JSON, "text/plain");
    harness.mine(2);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let view = reader.block_view(harness.height()).unwrap().unwrap();
    assert_eq!(view.height, harness.height());
    assert!(view.inscriptions.records.is_empty());
    assert!(view.inscriptions.infos.is_empty());
    stop(source);
}

#[test]
fn block_view_above_tip_is_none() {
    let harness = Harness::new();
    harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let tip = reader.tip().unwrap().unwrap();
    assert!(reader.block_view(tip + 1).unwrap().is_none());
    assert!(reader.block_view(tip).unwrap().is_some());
    stop(source);
}
