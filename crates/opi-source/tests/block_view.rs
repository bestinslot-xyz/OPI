mod support;

use {
    opi_core::{
        BlockSource, BlockView, InscriptionFeed, RuneCounter, RuneEtching, RuneEvent,
        RuneEventKind, RuneFeed,
    },
    opi_source::journal::{
        JournalReader, cf, encode_rune_counter_log, encode_rune_etching, encode_rune_event,
        rune_counter_log_key, rune_etching_key, rune_event_key, table,
    },
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
                runes: RuneFeed::default(),
            }
        );
    }
    assert!(checked >= 2);
    stop(source);
}

const TXID: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn event(kind: RuneEventKind, rune_id: &str, amount: u128) -> RuneEvent {
    RuneEvent {
        kind,
        txid: TXID.to_owned(),
        outpoint: Some(format!("{TXID}:1")),
        pkscript: Some(vec![0x51]),
        rune_id: rune_id.to_owned(),
        amount,
    }
}

fn etching(rune_id: &str, number: u64) -> RuneEtching {
    RuneEtching {
        rune_id: rune_id.to_owned(),
        block: rune_id.split(':').next().unwrap().parse().unwrap(),
        divisibility: 2,
        etching: TXID.to_owned(),
        terms: None,
        number,
        premine: 10,
        rune: "AB".to_owned(),
        spacers: 1,
        symbol: Some('x'),
        timestamp: 5,
        turbo: false,
    }
}

#[test]
fn block_view_reads_rune_feed_in_key_order() {
    let harness = Harness::new();
    harness.mine(2);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    let height = harness.height();

    let events = [
        event(RuneEventKind::Input, "9:1", 3),
        event(RuneEventKind::Output, "2:0", 4),
        event(RuneEventKind::Burn, "2:0", u128::MAX),
    ];
    let etchings = [etching("9:1", 0), etching("2:0", 1)];
    // Counter rows are inserted out of rune id order and reflect the after totals.
    let counters = [
        ("9:1", None, (5u128, 6u128)),
        ("2:10", Some((1, 1)), (2, 3)),
        ("2:9", None, (0, 1)),
    ];
    let tx = reader.journal().begin_write().unwrap();
    {
        let mut table_events = tx.open_table(table(cf::RUNE_EVENTS)).unwrap();
        let mut table_etchings = tx.open_table(table(cf::RUNE_ETCHINGS)).unwrap();
        let mut log = tx.open_table(table(cf::RUNE_COUNTER_LOG)).unwrap();
        // Neighbouring heights must not leak into the view.
        for other in [height - 1, height + 1] {
            table_events
                .insert(
                    rune_event_key(other, 0).as_slice(),
                    encode_rune_event(&event(RuneEventKind::Mint, "7:7", 1))
                        .unwrap()
                        .as_slice(),
                )
                .unwrap();
            log.insert(
                rune_counter_log_key(other, "7:7").unwrap().as_slice(),
                encode_rune_counter_log(None, (1, 1)).as_slice(),
            )
            .unwrap();
        }
        for (seq, e) in events.iter().enumerate().rev() {
            table_events
                .insert(
                    rune_event_key(height, seq as u32).as_slice(),
                    encode_rune_event(e).unwrap().as_slice(),
                )
                .unwrap();
        }
        for (seq, e) in etchings.iter().enumerate() {
            table_etchings
                .insert(
                    rune_etching_key(height, seq as u32).as_slice(),
                    encode_rune_etching(e).unwrap().as_slice(),
                )
                .unwrap();
        }
        for (rune_id, before, after) in counters {
            log.insert(
                rune_counter_log_key(height, rune_id).unwrap().as_slice(),
                encode_rune_counter_log(before, after).as_slice(),
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();

    let view = reader.block_view(height).unwrap().unwrap();
    assert_eq!(view.runes.events, events);
    assert_eq!(view.runes.etchings, etchings);
    assert_eq!(
        view.runes.counters,
        vec![
            RuneCounter {
                rune_id: "2:9".to_owned(),
                burned: 0,
                mints: 1
            },
            RuneCounter {
                rune_id: "2:10".to_owned(),
                burned: 2,
                mints: 3
            },
            RuneCounter {
                rune_id: "9:1".to_owned(),
                burned: 5,
                mints: 6
            },
        ]
    );
    stop(source);
}

#[test]
fn block_view_rune_feed_is_empty_without_rows() {
    let harness = Harness::new();
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    let view = reader.block_view(harness.height()).unwrap().unwrap();
    assert_eq!(view.runes, RuneFeed::default());
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
