mod support;

use {
    bitcoin::{OutPoint, ScriptBuf, WPubkeyHash, hashes::Hash},
    opi_core::{BlockSource, InscriptionInfo, InscriptionRecord},
    opi_source::{
        driver::{SourceHandle, spawn_source},
        journal::JournalReader,
    },
    ord::InscriptionId,
    support::{Faults, Harness, Shim, envelope, envelope_with, source_config, wait_for_tip},
};

const JSON: &[u8] = br#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"21000000"}"#;
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const REGTEST_JUBILEE_HEIGHT: u32 = 110;

/// Runs a source over the harness chain until its tip is the harness tip.
struct Run {
    _tmp: tempfile::TempDir,
    source: Option<SourceHandle>,
    reader: JournalReader,
}

impl Run {
    fn new(harness: &Harness) -> Run {
        Run::with_url(harness, harness.core.url())
    }

    /// Like [`Run::new`], reading bitcoind at `url`.
    fn with_url(harness: &Harness, url: String) -> Run {
        let tmp = tempfile::tempdir().unwrap();
        let source = spawn_source(source_config(tmp.path(), url, 2)).unwrap();
        let run = Run {
            reader: source.reader(),
            _tmp: tmp,
            source: Some(source),
        };
        wait_for_tip(&run.reader, harness.height());
        run
    }

    fn records(&self, height: u32) -> Vec<InscriptionRecord> {
        self.reader.inscription_records(height).unwrap()
    }

    /// The info of `id` among the infos the block at `height` carries.
    fn info(&self, height: u32, id: InscriptionId) -> Option<InscriptionInfo> {
        self.reader
            .block_view(height)
            .unwrap()
            .unwrap()
            .inscriptions
            .infos
            .get(&id.to_string())
            .cloned()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.shutdown();
            let result = source.join();
            if std::thread::panicking() {
                if let Err(err) = result {
                    eprintln!("source failed: {err:#}");
                }
            } else {
                result.unwrap();
            }
        }
    }
}

fn reveal_outpoint(id: InscriptionId) -> OutPoint {
    OutPoint {
        txid: id.txid,
        vout: 0,
    }
}

#[test]
fn creation_is_recorded_with_null_old_satpoint() {
    let harness = Harness::new();
    let id = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.idx, 1);
    assert_eq!(record.inscription_id, id.to_string());
    assert_eq!(record.old_satpoint, None);
    assert_eq!(record.new_satpoint, format!("{}:0:0", id.txid));
    assert_eq!(record.txid, id.txid.to_string());
    assert!(!record.sent_as_fee);

    let info = run.info(height, id).unwrap();
    assert!(info.is_json);
    assert!(!info.cursed_for_brc20);
    assert_eq!(info.inscription_number, 0);
    assert_eq!(info.parent_id, None);
    assert_eq!(info.content, JSON);
    assert_eq!(info.content_type, b"text/plain");

    let block = run.reader.block_info(height).unwrap().unwrap();
    assert_eq!(block.height, height);
    assert_eq!(
        block.hash,
        harness.core.state().hashes[height as usize].to_string()
    );
}

#[test]
fn transfer_records_spending_tx_and_pkscript() {
    let harness = Harness::new();
    let id = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let script = ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]));
    let txid = harness.transfer_to(reveal_outpoint(id), &script);
    let block = harness.mine(1).remove(0);
    let height = harness.height();

    let run = Run::new(&harness);

    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.idx, 1);
    assert_eq!(record.inscription_id, id.to_string());
    assert_eq!(record.old_satpoint, Some(format!("{}:0:0", id.txid)));
    assert_eq!(record.new_satpoint, format!("{txid}:0:0"));
    assert_eq!(record.txid, txid.to_string());
    assert_eq!(record.new_pkscript, script.to_bytes());
    let output = &block.txdata[1].output[0];
    assert_eq!(record.new_output_value, output.value.to_sat());
    assert!(!record.sent_as_fee);
}

#[test]
fn fee_spend_is_sent_as_fee_with_coinbase_satpoint() {
    let harness = Harness::new();
    let id = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let spend = harness.spend_to_fee(reveal_outpoint(id));
    let block = harness.mine(1).remove(0);
    let height = harness.height();
    let coinbase = &block.txdata[0];

    let run = Run::new(&harness);

    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert!(record.sent_as_fee);
    assert!(
        record
            .new_satpoint
            .starts_with(&format!("{}:0:", coinbase.compute_txid())),
        "{}",
        record.new_satpoint
    );
    assert_eq!(record.txid, spend.to_string());
    assert_eq!(
        record.new_pkscript,
        coinbase.output[0].script_pubkey.to_bytes()
    );
    assert_eq!(record.new_output_value, coinbase.output[0].value.to_sat());
}

#[test]
fn lost_inscription_is_sent_as_fee_with_null_satpoint() {
    let harness = Harness::new();
    let id = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let spend = harness.spend_to_fee(reveal_outpoint(id));
    harness.mine_with_subsidy(0);
    let height = harness.height();

    let run = Run::new(&harness);

    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert!(record.sent_as_fee);
    assert_eq!(
        record.new_satpoint,
        format!("{}:{}:0", bitcoin::Txid::all_zeros(), u32::MAX)
    );
    assert_eq!(record.txid, spend.to_string());
    assert!(record.new_pkscript.is_empty());
    assert_eq!(record.new_output_value, 0);
}

#[test]
fn image_inscription_is_untracked() {
    let harness = Harness::new();
    let id = harness.inscribe(PNG, "image/png");
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    assert_eq!(run.records(height), Vec::new());
    assert_eq!(run.info(height, id), None);
}

#[test]
fn unbound_inscription_is_untracked() {
    let harness = Harness::new();
    let id = harness.inscribe_witness(unbound_envelope());
    harness.mine(1);
    let height = harness.height();
    let shim = Shim::start(harness.core.url(), Faults::default());

    let run = Run::with_url(&harness, shim.url.clone());

    assert_eq!(run.records(height), Vec::new());
    assert_eq!(run.info(height, id), None);
    // No tracked event follows the lone unbound creation, so its undecided
    // fee flag can't move an idx and its input values aren't fetched.
    assert_eq!(shim.prevout_blocks(), 0);
}

/// A JSON envelope with an unrecognized even field, which leaves the
/// inscription unbound.
fn unbound_envelope() -> bitcoin::Witness {
    envelope_with(&[(&[1], b"text/plain"), (&[22], b"x")], JSON)
}

/// A block whose tx 1 reveals an unbound inscription, to fees when
/// `unbound_to_fee`, and whose tx 2 sends tracked inscription A to fees.
/// Event order alone can't tell whether the unbound one reserved an idx.
/// Returns A's idx.
fn idx_after_unbound_creation(unbound_to_fee: bool) -> u32 {
    let harness = Harness::new();
    harness.mine(2);
    let a = harness.reveal(1, envelope(b"text/plain", JSON));
    harness.mine(1);
    if unbound_to_fee {
        harness.reveal_to_fee(2, unbound_envelope());
    } else {
        harness.reveal(2, unbound_envelope());
    }
    harness.spend_to_fee(reveal_outpoint(a));
    harness.mine(1);
    let height = harness.height();
    let shim = Shim::start(harness.core.url(), Faults::default());

    let run = Run::with_url(&harness, shim.url.clone());

    assert_eq!(shim.prevout_blocks(), 1);
    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].inscription_id, a.to_string());
    assert!(records[0].sent_as_fee);
    records[0].idx
}

#[test]
fn unbound_fee_bound_creation_reserves_idx() {
    assert_eq!(idx_after_unbound_creation(true), 2);
}

#[test]
fn unbound_creation_kept_in_tx_reserves_no_idx() {
    assert_eq!(idx_after_unbound_creation(false), 1);
}

/// An envelope with its content type tag twice: `Curse::DuplicateField`.
fn duplicate_field_envelope() -> bitcoin::Witness {
    envelope_with(&[(&[1], b"text/plain"), (&[1], b"text/plain")], JSON)
}

#[test]
fn cursed_inscription_sets_cursed_for_brc20() {
    let harness = Harness::new();
    let id = harness.inscribe_witness(duplicate_field_envelope());
    harness.mine(1);
    assert!(harness.height() < REGTEST_JUBILEE_HEIGHT);

    let run = Run::new(&harness);

    let info = run.info(harness.height(), id).unwrap();
    assert!(info.cursed_for_brc20);
    assert!(info.inscription_number < 0);
}

#[test]
fn vindicated_inscription_sets_cursed_for_brc20() {
    let harness = Harness::new();
    harness.mine(REGTEST_JUBILEE_HEIGHT as usize);
    let id = harness.inscribe_witness(duplicate_field_envelope());
    harness.mine(1);
    assert!(harness.height() >= REGTEST_JUBILEE_HEIGHT);

    let run = Run::new(&harness);

    let info = run.info(harness.height(), id).unwrap();
    assert!(info.cursed_for_brc20);
    assert!(info.inscription_number >= 0);
}

#[test]
fn fee_bound_idx_matches_fork() {
    let harness = Harness::new();
    let a = harness.inscribe(JSON, "text/plain");
    let b = harness.inscribe(JSON, "text/plain");
    let image = harness.inscribe(PNG, "image/png");
    harness.mine(1);
    harness.spend_to_fee(reveal_outpoint(a));
    let move_b = harness.transfer(reveal_outpoint(b), 0);
    harness.spend_to_fee(reveal_outpoint(image));
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    let records = run.records(height);
    let ids = records
        .iter()
        .map(|r| (r.inscription_id.clone(), r.idx, r.sent_as_fee))
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![(a.to_string(), 1, true), (b.to_string(), 2, false)]
    );
    assert_eq!(records[1].txid, move_b.to_string());
}

#[test]
fn reorg_replaces_records() {
    let harness = Harness::new();
    harness.mine(2);
    let a = harness.reveal(1, envelope(b"text/plain", JSON));
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);
    let old_hash = run.reader.block_info(height).unwrap().unwrap().hash;
    let records = run.records(height);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].inscription_id, a.to_string());

    harness.invalidate_tip();
    let b = harness.reveal(2, envelope(b"application/json", br#"{"b":1}"#));
    harness.mine(2);
    let new_hash = harness.core.state().hashes[height as usize].to_string();
    assert_ne!(old_hash, new_hash);

    wait_for_tip(&run.reader, height + 1);

    assert_eq!(
        run.reader.block_info(height).unwrap().unwrap().hash,
        new_hash
    );
    let records = run.records(height);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].inscription_id, b.to_string());
    assert_eq!(run.info(height, a), None);
    assert!(run.info(height, b).is_some());
    assert_eq!(run.reader.journal().tracked(a).unwrap(), None);
    assert!(run.reader.journal().tracked(b).unwrap().is_some());
}

fn script(byte: u8) -> ScriptBuf {
    ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([byte; 20]))
}

/// `(inscription id, idx, txid)` of every record at `height`.
fn summary(run: &Run, height: u32) -> Vec<(String, u32, String)> {
    run.records(height)
        .into_iter()
        .map(|r| (r.inscription_id, r.idx, r.txid))
        .collect()
}

#[test]
fn only_first_transfer_is_recorded() {
    let harness = Harness::new();
    let a = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let created_at = harness.height();
    let first = harness.transfer_to(reveal_outpoint(a), &script(7));
    harness.mine(1);
    let first_at = harness.height();
    let second = harness.transfer_to(OutPoint::new(first, 0), &script(7));
    harness.mine(1);
    let third = harness.transfer_to(OutPoint::new(second, 0), &script(7));
    harness.mine(1);

    let run = Run::new(&harness);

    assert_eq!(run.records(created_at).len(), 1);
    assert_eq!(
        summary(&run, first_at),
        vec![(a.to_string(), 1, first.to_string())]
    );
    for height in first_at + 1..=harness.height() {
        assert_eq!(run.records(height), Vec::new(), "height {height}");
    }
    assert_ne!(second, third);
}

#[test]
fn later_transfer_takes_no_idx() {
    let harness = Harness::new();
    let a = harness.inscribe(JSON, "text/plain");
    let b = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let first = harness.transfer_to(reveal_outpoint(a), &script(7));
    harness.mine(1);
    harness.transfer_to(OutPoint::new(first, 0), &script(7));
    let move_b = harness.transfer_to(reveal_outpoint(b), &script(7));
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    assert_eq!(
        summary(&run, height),
        vec![(b.to_string(), 1, move_b.to_string())]
    );
}

#[test]
fn same_block_create_then_transfer() {
    let harness = Harness::new();
    harness.mine(1);
    let a = harness.reveal(1, envelope(b"text/plain", JSON));
    let moved = harness.spend_in_mempool(reveal_outpoint(a));
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    let records = run.records(height);
    assert_eq!(
        summary(&run, height),
        vec![
            (a.to_string(), 1, a.txid.to_string()),
            (a.to_string(), 2, moved.to_string()),
        ]
    );
    assert_eq!(records[0].old_satpoint, None);
    assert_eq!(records[1].old_satpoint, Some(format!("{}:0:0", a.txid)));
}

#[test]
fn untracked_fee_spend_before_tracked_move_reserves_idx() {
    let harness = Harness::new();
    let image = harness.inscribe(PNG, "image/png");
    let b = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    harness.spend_to_fee(reveal_outpoint(image));
    let move_b = harness.transfer(reveal_outpoint(b), 0);
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);

    assert_eq!(
        summary(&run, height),
        vec![(b.to_string(), 2, move_b.to_string())]
    );
}

#[test]
fn reorged_first_transfer_is_recorded_again() {
    let harness = Harness::new();
    let a = harness.inscribe(JSON, "text/plain");
    harness.mine(1);
    let first = harness.transfer_to(reveal_outpoint(a), &script(7));
    harness.mine(1);
    let height = harness.height();

    let run = Run::new(&harness);
    assert_eq!(
        summary(&run, height),
        vec![(a.to_string(), 1, first.to_string())]
    );

    harness.invalidate_tip();
    harness.restore_utxo(reveal_outpoint(a));
    let replacement = harness.transfer_to(reveal_outpoint(a), &script(8));
    harness.mine(2);
    assert_ne!(first, replacement);

    wait_for_tip(&run.reader, height + 1);

    assert_eq!(
        summary(&run, height),
        vec![(a.to_string(), 1, replacement.to_string())]
    );
    assert_eq!(run.records(height)[0].new_pkscript, script(8).to_bytes());
}
