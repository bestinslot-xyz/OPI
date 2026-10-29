mod support;

use {
    bitcoin::OutPoint,
    opi_core::BlockSource,
    opi_source::journal::{
        Journal, PRUNE_STEP_ROWS, PruneStep, cf, info_key, table,
        testing::{self, Density},
    },
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
    Journal::compact(&path, &Default::default()).unwrap();
    let journal = Journal::open(&path, ord::Chain::Regtest, true).unwrap();
    assert_eq!(journal.pruned_below().unwrap(), floor);
    let tx = journal.db().begin_read().unwrap();
    assert!(tx.open_table(table(cf::HEADERS)).unwrap().len().unwrap() > 0);
}

fn rows_at(journal: &Journal, name: &'static str, height: u32) -> u64 {
    let tx = journal.db().begin_read().unwrap();
    let rows = tx.open_table(table(name)).unwrap();
    let (first, end) = (height.to_be_bytes(), (height + 1).to_be_bytes());
    rows.range::<&[u8]>(first.as_slice()..end.as_slice())
        .unwrap()
        .count() as u64
}

fn rows(journal: &Journal, name: &'static str) -> u64 {
    let tx = journal.db().begin_read().unwrap();
    tx.open_table(table(name)).unwrap().len().unwrap()
}

/// Each step deletes at most its row budget of raw events and at most its
/// budget of records; the steps together prune exactly to the bounds.
#[test]
fn prune_steps_stay_within_the_row_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let journal =
        Journal::open(&tmp.path().join("journal.redb"), ord::Chain::Regtest, false).unwrap();
    let density = Density {
        raw_events: 300,
        creations: 100,
        transfers: 60,
        content: 64,
    };
    testing::fill(&journal, 10..60, density).unwrap();
    let per_height_records = u64::from(density.creations + density.transfers);
    let (raw_below, records_below) = (50, 40);
    let budget = 1000;

    let mut steps = 0;
    loop {
        let step = journal
            .prune_step(raw_below, records_below, budget)
            .unwrap();
        steps += 1;
        assert!(step.pruned.raw_events <= budget, "{step:?}");
        assert!(
            step.pruned.transfers + step.pruned.infos <= budget,
            "{step:?}"
        );
        // Every height from the floor up is whole; a height left in part is
        // already below it.
        let floor = journal.pruned_below().unwrap();
        let records_floor = journal.records_floor().unwrap();
        assert!(records_floor <= floor && floor - records_floor <= 1);
        assert_eq!(rows_below(&journal, cf::TRANSFERS, records_floor), 0);
        if (11..records_below).contains(&floor) {
            assert_eq!(rows_at(&journal, cf::TRANSFERS, floor), per_height_records);
        }
        let raw_floor = journal.raw_floor().unwrap().unwrap();
        assert_eq!(rows_below(&journal, cf::RAW_EVENTS, raw_floor), 0);
        assert_eq!(
            step.done,
            records_floor == records_below && raw_floor == raw_below
        );
        if step.done {
            break;
        }
    }
    assert!(steps >= 10, "{steps} steps");
    assert_eq!(journal.pruned_below().unwrap(), records_below);
    assert_eq!(journal.raw_floor().unwrap(), Some(raw_below));
    assert_eq!(
        rows(&journal, cf::RAW_EVENTS),
        u64::from(density.raw_events) * 10
    );
    assert_eq!(rows(&journal, cf::TRANSFERS), per_height_records * 20);
    assert!(
        journal
            .prune_step(raw_below, records_below, budget)
            .unwrap()
            == PruneStep {
                pruned: Default::default(),
                done: true
            }
    );
    let kept = testing::inscription_id(40, 0);
    let consumed = testing::inscription_id(38, 0);
    assert!(has_info_str(&journal, &kept));
    assert!(!has_info_str(&journal, &consumed));
}

/// A height with more raw events than the budget is pruned over several
/// steps: each deletes at most the budget, the first already drops the
/// height's tag so nothing reads the rest as its block's log, and the raw
/// floor stays at the height until its last event goes.
#[test]
fn a_raw_height_larger_than_the_budget_prunes_over_several_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let journal =
        Journal::open(&tmp.path().join("journal.redb"), ord::Chain::Regtest, false).unwrap();
    let density = Density {
        raw_events: 2500,
        creations: 1,
        transfers: 1,
        content: 8,
    };
    testing::fill(&journal, 0..5, density).unwrap();
    let step = journal.prune_step(3, 0, 1000).unwrap();
    assert_eq!(step.pruned.raw_events, 1000);
    assert!(!step.done);
    assert_eq!(journal.raw_floor().unwrap(), Some(0));
    assert_eq!(rows_at(&journal, cf::RAW_EVENTS, 0), 1500);
    assert_eq!(rows_at(&journal, cf::RAW_HEIGHTS, 0), 0);
    assert_eq!(rows_at(&journal, cf::RAW_HEIGHTS, 1), 1);

    let mut steps = 1;
    while !journal.prune_step(3, 0, 1000).unwrap().done {
        steps += 1;
    }
    assert_eq!(
        steps, 7,
        "7500 events in steps of 1000, the last one partial"
    );
    assert_eq!(journal.raw_floor().unwrap(), Some(3));
    assert_eq!(rows(&journal, cf::RAW_EVENTS), 2 * 2500);
    assert_eq!(rows(&journal, cf::RAW_HEIGHTS), 2);
    assert!(journal.prune_step(3, 0, 0).is_err());
}

/// A height with more records than the budget is pruned over several
/// transactions. The first that leaves it in part already puts it below the
/// pruned floor, so the reader refuses it; a later step resumes from its
/// lowest row left, deletes each consumed info row with its transfer, and
/// ends at the bounds, keeping every height at or above them whole.
#[test]
fn a_records_height_larger_than_the_budget_prunes_over_several_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let journal =
        Journal::open(&tmp.path().join("journal.redb"), ord::Chain::Regtest, false).unwrap();
    let density = Density {
        raw_events: 1,
        creations: 1500,
        transfers: 1000,
        content: 8,
    };
    testing::fill(&journal, 0..5, density).unwrap();
    let reader = opi_source::journal::JournalReader::new(journal.clone());
    let per_height = u64::from(density.creations + density.transfers);
    assert_eq!(
        reader.inscription_records(1).unwrap().len() as u64,
        per_height
    );

    // Height 0 holds 1500 creations; height 1 holds 1500 creations, then
    // 1000 first transfers that each consume an info row of height 0.
    let step = journal.prune_step(0, 3, 1000).unwrap();
    assert!(!step.done);
    assert_eq!(step.pruned.transfers, 1000);
    assert_eq!(
        journal.pruned_below().unwrap(),
        1,
        "height 0 is left in part"
    );
    assert_eq!(journal.records_floor().unwrap(), 0);
    assert_eq!(rows_at(&journal, cf::TRANSFERS, 0), 500);
    let err = reader.inscription_records(0).unwrap_err();
    assert!(format!("{err:#}").contains("pruned floor"), "{err:#}");

    let mut steps = 1;
    loop {
        let step = journal.prune_step(0, 3, 1000).unwrap();
        assert!(
            step.pruned.transfers + step.pruned.infos <= 1000,
            "{step:?}"
        );
        let floor = journal.pruned_below().unwrap();
        assert!(reader.inscription_records(floor.saturating_sub(1)).is_err() || floor == 0);
        steps += 1;
        if step.done {
            break;
        }
    }
    assert!(steps >= 6, "{steps} steps");
    assert_eq!(journal.pruned_below().unwrap(), 3);
    assert_eq!(journal.records_floor().unwrap(), 3);
    assert_eq!(rows_below(&journal, cf::TRANSFERS, 3), 0);
    for height in 3..5 {
        assert_eq!(rows_at(&journal, cf::TRANSFERS, height), per_height);
        assert_eq!(
            reader.inscription_records(height).unwrap().len() as u64,
            per_height
        );
    }
    // Content first transferred below the floor went with its transfer;
    // content transferred at the floor, or never, stays.
    assert!(!has_info_str(&journal, &testing::inscription_id(1, 0)));
    assert!(has_info_str(&journal, &testing::inscription_id(2, 0)));
    assert!(has_info_str(&journal, &testing::inscription_id(1, 1499)));
}

fn has_info_str(journal: &Journal, id: &str) -> bool {
    let tx = journal.db().begin_read().unwrap();
    let infos = tx.open_table(table(cf::INFO)).unwrap();
    infos
        .get(info_key(id).unwrap().as_slice())
        .unwrap()
        .is_some()
}

/// Prunes a dense synthetic journal with the heights-only steps and with the
/// row-bounded steps, printing the longest step and the peak file length of
/// each. Sized by OPI_MEASURE_HEIGHTS (default 300); run with
/// `cargo test --release -p opi-source --test prune measure -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_dense_prune() {
    let heights: u32 = std::env::var("OPI_MEASURE_HEIGHTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);
    // Late-2023 mainnet blocks: thousands of inscriptions, most of them
    // BRC-20 transfers moved again within blocks.
    let density = Density {
        raw_events: 8000,
        creations: 3000,
        transfers: 2000,
        content: 300,
    };
    let budgets: Vec<u64> = std::env::var("OPI_MEASURE_BUDGETS")
        .ok()
        .map(|budgets| {
            budgets
                .split(',')
                .map(|budget| budget.parse().unwrap_or(u64::MAX))
                .collect()
        })
        .unwrap_or_else(|| vec![u64::MAX, PRUNE_STEP_ROWS]);
    for budget in budgets {
        let label = match budget {
            u64::MAX => "heights only".to_string(),
            rows => format!("{rows} rows"),
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal.redb");
        let journal = Journal::open(&path, ord::Chain::Regtest, false).unwrap();
        let filled = std::time::Instant::now();
        testing::fill(&journal, 0..heights, density).unwrap();
        let before = std::fs::metadata(&path).unwrap().len();
        eprintln!(
            "{label}: filled {heights} heights in {:?}, file {} MiB",
            filled.elapsed(),
            before >> 20
        );

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = std::thread::spawn({
            let (stop, path) = (stop.clone(), path.clone());
            move || {
                let mut peak = 0;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    peak = peak.max(std::fs::metadata(&path).unwrap().len());
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                peak
            }
        });
        let started = std::time::Instant::now();
        let (mut steps, mut longest) = (0, std::time::Duration::ZERO);
        loop {
            let step_started = std::time::Instant::now();
            let step = journal.prune_step(heights, heights, budget).unwrap();
            longest = longest.max(step_started.elapsed());
            steps += 1;
            if step.done {
                break;
            }
        }
        let total = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let peak = watcher
            .join()
            .unwrap()
            .max(std::fs::metadata(&path).unwrap().len());
        eprintln!(
            "{label}: {steps} steps in {total:?}, longest {longest:?}, file {} MiB before, \
             peak {} MiB ({:.2}x)",
            before >> 20,
            peak >> 20,
            peak as f64 / before as f64
        );
    }
}
