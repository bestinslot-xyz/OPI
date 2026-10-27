mod support;

use {
    bitcoin::OutPoint,
    opi_core::BlockSource,
    opi_source::{driver::spawn_source, journal::Journal},
    ord::Chain,
    std::time::Duration,
    support::{
        Faults, Harness, Shim, dump, envelope, envelope_with, source_config, start, wait_for_tip,
    },
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"mint","tick":"ordi","amt":"1000"}"#;

/// A chain of `blocks` blocks with an inscription revealed at every even
/// height from 2.
fn chain_with_inscriptions(blocks: u32) -> Harness {
    let harness = Harness::new();
    while harness.height() + 2 <= blocks {
        harness.inscribe(JSON, "text/plain");
        harness.mine(1);
    }
    let rest = blocks - harness.height();
    harness.mine(rest as usize);
    assert_eq!(harness.height(), blocks);
    harness
}

#[test]
fn driver_retries_after_upstream_errors() {
    let harness = chain_with_inscriptions(6);
    let target = harness.core.state().hashes[4];
    let shim = Shim::start(
        harness.core.url(),
        Faults {
            // ord's block fetcher retries these itself.
            fail_getblock: Some((target, 2)),
            // These fail ord's `update()`, so the driver retries it.
            fail_blockchaininfo: Some((4, 2)),
            ..Faults::default()
        },
    );
    let tmp = tempfile::tempdir().unwrap();

    let source = spawn_source(source_config(tmp.path(), shim.url.clone(), 2)).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    assert_eq!(shim.pending_failures(), (0, 0, 0));

    for height in 0..=harness.height() {
        let block = reader.block_info(height).unwrap().unwrap();
        assert_eq!(
            block.hash,
            harness.core.state().hashes[height as usize].to_string()
        );
    }
    for height in [2, 4, 6] {
        assert_eq!(reader.inscription_records(height).unwrap().len(), 1);
    }

    source.shutdown();
    source.join().unwrap();
}

#[test]
fn startup_refuses_when_ord_is_ahead_of_raw_log() {
    let harness = chain_with_inscriptions(4);
    let tmp = tempfile::tempdir().unwrap();
    let source = start(&harness, tmp.path());
    wait_for_tip(&source.reader(), harness.height());
    source.shutdown();
    source.join().unwrap();

    std::fs::remove_dir_all(tmp.path().join("journal")).unwrap();

    for _ in 0..2 {
        let err = spawn_source(source_config(tmp.path(), harness.core.url(), 2))
            .err()
            .expect("startup refuses");
        assert!(format!("{err:#}").contains("raw event log"), "{err:#}");
    }
}

#[test]
fn startup_refuses_a_restored_older_journal() {
    let harness = chain_with_inscriptions(4);
    let tmp = tempfile::tempdir().unwrap();
    let config = source_config(tmp.path(), harness.core.url(), 2);
    let journal_path = config.journal_path();
    let source = spawn_source(config.clone()).unwrap();
    wait_for_tip(&source.reader(), harness.height());
    source.shutdown();
    source.join().unwrap();
    let backup = tmp.path().join("journal-backup.redb");
    std::fs::copy(&journal_path, &backup).unwrap();

    harness.inscribe(JSON, "text/plain");
    harness.mine(3);
    let source = spawn_source(config.clone()).unwrap();
    wait_for_tip(&source.reader(), harness.height());
    source.shutdown();
    source.join().unwrap();

    std::fs::copy(&backup, &journal_path).unwrap();
    let err = spawn_source(config).err().expect("startup refuses");
    assert!(format!("{err:#}").contains("raw event log"), "{err:#}");
}

#[test]
fn finalize_retries_after_upstream_errors() {
    let harness = Harness::new();
    harness.mine(2);
    let a = harness.reveal(1, envelope(b"text/plain", JSON));
    harness.mine(1);
    // Tx 1 sends an unbound inscription to fees ahead of tracked A, so
    // finalize reads the block's input values.
    harness.reveal_to_fee(
        2,
        envelope_with(&[(&[1], b"text/plain"), (&[22], b"x")], JSON),
    );
    harness.spend_to_fee(OutPoint {
        txid: a.txid,
        vout: 0,
    });
    harness.mine(1);
    let height = harness.height();
    let shim = Shim::start(
        harness.core.url(),
        Faults {
            fail_prevout_blocks: 2,
            ..Faults::default()
        },
    );
    let tmp = tempfile::tempdir().unwrap();

    let source = spawn_source(source_config(tmp.path(), shim.url.clone(), 2)).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, height);

    assert_eq!(shim.pending_failures(), (0, 0, 0));
    assert_eq!(shim.prevout_blocks(), 1);
    let records = reader.inscription_records(height).unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].idx, 2);

    source.shutdown();
    source.join().unwrap();
}

#[test]
fn journal_open_recovers_from_interrupted_init() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal.redb");
    let init = tmp.path().join("journal.redb.init");
    std::fs::write(&init, b"torn garbage").unwrap();

    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert!(journal.created());
    drop(journal);
    assert!(!init.exists());

    let journal = Journal::open(&path, Chain::Regtest, true).unwrap();
    assert!(!journal.created());
}

#[test]
fn ord_index_init_is_atomic() {
    let harness = chain_with_inscriptions(4);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ord").join("regtest");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.redb.init"), b"torn garbage").unwrap();

    let source = start(&harness, tmp.path());
    wait_for_tip(&source.reader(), harness.height());
    source.shutdown();
    source.join().unwrap();

    assert!(dir.join("index.redb").exists());
    assert!(!dir.join("index.redb.init").exists());
}

#[test]
fn startup_refuses_on_chain_mismatch() {
    let harness = chain_with_inscriptions(2);
    let tmp = tempfile::tempdir().unwrap();
    let source = start(&harness, tmp.path());
    wait_for_tip(&source.reader(), harness.height());
    source.shutdown();
    source.join().unwrap();

    let mut config = source_config(tmp.path(), harness.core.url(), 2);
    config.chain = Chain::Signet;
    let err = spawn_source(config).err().expect("startup refuses");
    assert!(format!("{err:#}").contains("chain"), "{err:#}");
}

#[test]
fn shutdown_mid_chunk_resumes_cleanly() {
    let harness = chain_with_inscriptions(30);
    let tip = harness.height();

    let uninterrupted = tempfile::tempdir().unwrap();
    let config = source_config(uninterrupted.path(), harness.core.url(), 10);
    let journal_path = config.journal_path();
    let source = spawn_source(config).unwrap();
    wait_for_tip(&source.reader(), tip);
    source.shutdown();
    source.join().unwrap();
    let expected = dump(&journal_path);

    let interrupted = tempfile::tempdir().unwrap();
    let shim = Shim::start(
        harness.core.url(),
        Faults {
            getblock_delay: Duration::from_millis(40),
            ..Faults::default()
        },
    );
    let config = source_config(interrupted.path(), shim.url.clone(), 10);
    let journal_path = config.journal_path();
    let source = spawn_source(config).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, 0);
    std::thread::sleep(Duration::from_millis(150));
    source.shutdown();
    source.join().unwrap();
    let stopped_at = reader.tip().unwrap().unwrap();
    assert!(stopped_at < tip, "shutdown landed after the last chunk");
    drop(reader);

    let source = spawn_source(source_config(interrupted.path(), harness.core.url(), 10)).unwrap();
    wait_for_tip(&source.reader(), tip);
    source.shutdown();
    source.join().unwrap();

    let actual = dump(&journal_path);
    assert!(!expected.is_empty());
    assert_eq!(actual, expected);
}
