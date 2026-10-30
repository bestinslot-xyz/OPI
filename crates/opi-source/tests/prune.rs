mod support;

use {
    bitcoin::OutPoint,
    opi_core::BlockSource,
    opi_source::{
        driver::{SourceConfig, spawn_source},
        journal::{
            Journal, JournalOptions, begin_write, cf, raw_event_key, rollback_to, segment_name,
            table,
            testing::{self, Density},
        },
    },
    ord::{Chain, InscriptionId},
    support::{Harness, segment_rows, source_config, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"transfer","tick":"ordi","amt":"10"}"#;

fn outpoint(id: InscriptionId) -> OutPoint {
    OutPoint {
        txid: id.txid,
        vout: 0,
    }
}

fn options(segment: u32) -> JournalOptions {
    JournalOptions {
        segment,
        ..JournalOptions::default()
    }
}

/// Rows of table `name` below `height` across the journal's segments.
fn rows_below(journal: &Journal, name: &'static str, height: u32) -> usize {
    segment_rows(journal, name)
        .iter()
        .filter(|(key, _)| u32::from_be_bytes(key[..4].try_into().unwrap()) < height)
        .count()
}

/// Pruning deletes the segments below both bounds, the reader refuses their
/// heights, and the tracked set keeps what later blocks need: an inscription
/// not yet transferred keeps its row, so its first transfer after the prune
/// is still recorded with its content.
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
    harness.mine(4);

    let tmp = tempfile::tempdir().unwrap();
    let config = SourceConfig {
        journal: options(2),
        ..source_config(tmp.path(), harness.core.url(), 2)
    };
    let source = spawn_source(config).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    let journal = reader.journal().clone();
    assert_eq!(reader.inscription_records(moved_at).unwrap().len(), 1);
    assert!(journal.tracked(moved).unwrap().is_some());

    let floor = moved_at + 1;
    let segments = journal.segment_starts().len();
    let pruned = journal.prune(floor, floor).unwrap();
    let below = floor - floor % 2;
    assert_eq!(pruned.segments, u64::from(below / 2), "{pruned:?}");
    assert_eq!(pruned.tracked, 1, "{pruned:?}");
    assert_eq!(
        journal.segment_starts().len(),
        segments - below as usize / 2
    );
    assert_eq!(journal.pruned_below().unwrap(), below);
    for start in (0..below).step_by(2) {
        assert!(!journal.dir().join(segment_name(start)).exists());
    }
    for name in [cf::RAW_EVENTS, cf::TRANSFERS, cf::HEADERS] {
        assert_eq!(rows_below(&journal, name, below), 0, "{name}");
    }
    assert_eq!(journal.tracked(moved).unwrap(), None);
    assert!(journal.tracked(kept).unwrap().is_some());

    let err = reader.block_view(below - 1).unwrap_err();
    assert!(format!("{err:#}").contains("pruned floor"), "{err:#}");
    assert!(reader.inscription_records(below - 1).is_err());
    assert!(reader.block_info(below - 1).unwrap().is_none());
    assert!(reader.block_view(below).unwrap().is_some());

    // A second prune at the same bounds finds nothing.
    assert!(journal.prune(floor, floor).unwrap().is_empty());
    assert!(journal.prune(harness.height() + 2, floor).is_err());

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
    let journal = Journal::open_with(
        &tmp.path().join("journal"),
        Chain::Regtest,
        true,
        &options(2),
    )
    .unwrap();
    assert_eq!(journal.pruned_below().unwrap(), below);
}

const SMALL: Density = Density {
    raw_events: 50,
    creations: 20,
    transfers: 10,
    content: 40,
};

/// A prune never deletes the segment holding the final tip, nor one with a
/// height at or above either bound.
#[test]
fn prune_deletes_whole_segments_below_both_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..450, SMALL).unwrap();
    assert_eq!(journal.segment_starts(), [0, 100, 200, 300, 400]);

    assert!(journal.prune(250, 150).unwrap().segments == 1);
    assert_eq!(journal.segment_starts(), [100, 200, 300, 400]);
    assert_eq!(journal.pruned_below().unwrap(), 100);

    assert!(journal.prune(450, 450).unwrap().segments == 3);
    assert_eq!(journal.segment_starts(), [400]);
    assert_eq!(journal.final_tip().unwrap(), Some(449));
}

/// A reader holding a segment keeps reading it after a prune deletes its
/// file.
#[test]
fn a_held_segment_outlives_its_file() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..250, SMALL).unwrap();
    let held = journal.segment(50).unwrap().unwrap();
    journal.prune(250, 250).unwrap();
    assert!(!tmp.path().join(segment_name(0)).exists());
    let tx = redb::ReadableDatabase::begin_read(&*held).unwrap();
    let transfers = tx.open_table(table(cf::TRANSFERS)).unwrap();
    assert_eq!(
        redb::ReadableTableMetadata::len(&transfers).unwrap(),
        100 * 30 - 10
    );
}

/// A rollback across a segment boundary deletes the rows above the agreed
/// height in both segments and lowers the final tip, and finalizing on top
/// of it works as before.
#[test]
fn rollback_crosses_a_segment_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..204, SMALL).unwrap();
    rollback_to(&journal, 198).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(198));
    for name in [cf::TRANSFERS, cf::INFO] {
        let rows = segment_rows(&journal, name);
        assert!(
            rows.iter()
                .all(|(key, _)| u32::from_be_bytes(key[..4].try_into().unwrap()) <= 198),
            "{name}"
        );
    }
    drop(journal);
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(198));
    testing::fill(&journal, 199..210, SMALL).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(209));
}

fn env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Writes the raw events of `height` in a transaction that never commits,
/// as a process killed mid-block leaves them.
fn killed_mid_block(journal: &Journal, height: u32, density: Density) {
    let segment = journal.segment_mut(height).unwrap();
    let tx = begin_write(&segment).unwrap();
    {
        let mut events = tx.open_table(table(cf::RAW_EVENTS)).unwrap();
        for seq in 0..density.raw_events / 2 {
            events
                .insert(raw_event_key(height, seq).as_slice(), [9u8; 120].as_slice())
                .unwrap();
        }
    }
    drop(tx);
}

/// Indexes many windows of craze-shaped blocks into a journal pruned to a
/// window of `keep` heights, with rollbacks across segment boundaries and
/// restarts after a block killed mid-write. After every prune the journal's
/// files stay under a bound set by one window: the window's segments, the
/// one being written and the one the window starts in, each at most twice
/// the file one full segment makes (redb doubles a file until it fills its
/// first 4 GiB region), plus the meta and tracked files. Which segment files
/// redb has doubled moves the size within the bound from window to window.
///
/// `OPI_PRUNE_PROPERTY_SCALE` multiplies the block size, for a heavier run
/// than CI's.
#[test]
fn disk_use_stays_bounded_across_windows() {
    let scale = env("OPI_PRUNE_PROPERTY_SCALE", 1);
    let density = Density {
        raw_events: 600 * scale,
        creations: 300 * scale,
        transfers: 150 * scale,
        content: 60,
    };
    let (segment, keep, windows) = (50, 200, 6);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("journal");
    let mut journal = Journal::open_with(&dir, Chain::Regtest, false, &options(segment)).unwrap();

    // The bytes one full segment stores, measured on its own.
    testing::fill(&journal, 0..segment, density).unwrap();
    let stored = {
        let segment = journal.segment(0).unwrap().unwrap();
        let tx = segment.begin_write().unwrap();
        let stats = tx.stats().unwrap();
        tx.abort().unwrap();
        stats.stored_bytes() + stats.metadata_bytes()
    };
    let full_segment = std::fs::metadata(dir.join(segment_name(0))).unwrap().len();
    let base = journal.disk_bytes().unwrap() - full_segment;
    let bound = (u64::from(keep / segment) + 2) * 2 * full_segment + 2 * base;

    let mut next = segment;
    let mut largest = 0;
    // The largest size within each window from the second on.
    let mut window_largest = std::collections::BTreeMap::<u32, u64>::new();
    let mut round = 0;
    while next < keep * windows {
        let step = 37 + round % 29;
        testing::fill(&journal, next..next + step, density).unwrap();
        next += step;
        round += 1;
        if round % 3 == 0 {
            // A reorg 3 to 12 blocks deep, across a boundary when it falls
            // near one.
            let depth = 3 + round % 10;
            let agreed = next - 1 - depth;
            rollback_to(&journal, agreed).unwrap();
            next = agreed + 1;
        }
        if round % 5 == 0 {
            killed_mid_block(&journal, next, density);
            drop(journal);
            journal = Journal::open_with(&dir, Chain::Regtest, false, &options(segment)).unwrap();
            assert_eq!(journal.final_tip().unwrap(), Some(next - 1));
        }
        let tip = journal.final_tip().unwrap().unwrap();
        let below = (tip + 1).saturating_sub(keep);
        journal.prune(below, below).unwrap();
        let disk = journal.disk_bytes().unwrap();
        largest = largest.max(disk);
        if tip >= 2 * keep {
            let window = window_largest.entry(tip / keep).or_default();
            *window = (*window).max(disk);
        }
        assert!(
            disk <= bound,
            "round {round}, tip {tip}: {disk} bytes on disk, bound {bound}, files {:?}",
            std::fs::read_dir(&dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), entry.metadata().unwrap().len())
                })
                .collect::<Vec<_>>()
        );
        assert!(
            journal.segment_starts().len() as u32 <= keep / segment + 2,
            "{:?}",
            journal.segment_starts()
        );
    }
    println!(
        "indexed {next} heights; largest journal {largest} bytes, bound {bound}, one segment \
         stores {stored} bytes in a {full_segment}-byte file; largest per window \
         {window_largest:?}"
    );
}
