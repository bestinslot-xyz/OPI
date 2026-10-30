mod support;

use {
    bitcoin::{OutPoint, Txid},
    opi_core::{BlockSource, RuneCounter, RuneEtching, RuneEvent, RuneEventKind},
    opi_source::{
        DEFAULT_ORD_CACHE_BYTES,
        driver::{SourceHandle, spawn_source},
        journal::JournalReader,
        ord_settings,
    },
    ord::{Chain, Index},
    ordinals::{Edict, Etching, Rune, RuneId, Runestone, Terms},
    std::{
        collections::{BTreeMap, HashMap},
        path::Path,
    },
    support::{Harness, RUNE_TABLES, envelope, segment_rows, source_config, start, wait_for_tip},
};

/// Pkscript and `(rune id, amount)` balances of an outpoint.
type RuneBalanceRow = (Vec<u8>, Vec<(String, u128)>);

/// Burned and mint totals of a rune.
type RuneTotals = (u128, u128);

/// A block's rune records as the runes module completes them from the
/// journal's feed: every event with the inputs, the etchings and the
/// counter rows.
struct Completed {
    events: Vec<RuneEvent>,
    etchings: Vec<RuneEtching>,
    counters: Vec<RuneCounter>,
}

/// One finalized block with its completed rune records.
struct View {
    time: u32,
    runes: Completed,
    inscriptions: opi_core::InscriptionFeed,
}

/// The runes module's state as it reads it to complete a block: unspent
/// rune outpoints and rune totals.
#[derive(Default)]
struct Model {
    held: HashMap<String, RuneBalanceRow>,
    totals: HashMap<String, RuneTotals>,
}

impl Model {
    /// Completes the journal's feed of `height` and applies it.
    fn apply(&mut self, reader: &JournalReader, height: u32) -> View {
        let view = reader.block_view(height).unwrap().unwrap();
        let feed = view.runes;
        let mut held = self.held.clone();
        for event in feed
            .events
            .iter()
            .filter(|e| e.kind == RuneEventKind::Output)
        {
            let outpoint = event.outpoint.clone().unwrap();
            let row = held
                .entry(outpoint)
                .or_insert_with(|| (event.pkscript.clone().unwrap(), Vec::new()));
            row.1.push((event.rune_id.clone(), event.amount));
        }
        let events = opi_runes::complete_events(&feed.events, &feed.spent, &held).unwrap();
        let counters = opi_runes::counters(&events, &self.totals).unwrap();
        for event in events.iter().filter(|e| e.kind == RuneEventKind::Input) {
            held.remove(event.outpoint.as_deref().unwrap());
        }
        self.held = held;
        for counter in &counters {
            self.totals
                .insert(counter.rune_id.clone(), (counter.burned, counter.mints));
        }
        View {
            time: view.time,
            runes: Completed {
                events,
                etchings: feed.etchings,
                counters,
            },
            inscriptions: view.inscriptions,
        }
    }

    /// The module's state after the journal's blocks up to `height`.
    fn through(reader: &JournalReader, height: u32) -> (Model, Option<View>) {
        let mut model = Model::default();
        let mut last = None;
        for at in 0..=height {
            last = Some(model.apply(reader, at));
        }
        (model, last)
    }
}

/// The completed block at `height`.
fn view(reader: &JournalReader, height: u32) -> View {
    Model::through(reader, height).1.unwrap()
}

/// The module's unspent rune outpoints after the journal's final tip.
fn journal_balances(reader: &JournalReader) -> BTreeMap<OutPoint, RuneBalanceRow> {
    let tip = reader.tip().unwrap().unwrap();
    Model::through(reader, tip)
        .0
        .held
        .into_iter()
        .map(|(outpoint, row)| (outpoint.parse().unwrap(), row))
        .collect()
}

/// The module's rune totals after the journal's final tip.
fn journal_counters(reader: &JournalReader) -> BTreeMap<String, RuneTotals> {
    let tip = reader.tip().unwrap().unwrap();
    Model::through(reader, tip).0.totals.into_iter().collect()
}

const JSON: &[u8] = br#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"21000000"}"#;

fn synced(harness: &Harness, dir: &Path) -> (SourceHandle, JournalReader) {
    let source = start(harness, dir);
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());
    (source, reader)
}

fn stop(source: SourceHandle) {
    source.shutdown();
    source.join().unwrap();
}

/// ord's index of a stopped source under `dir`.
fn ord_index(harness: &Harness, dir: &Path) -> Index {
    let settings = ord_settings(
        Chain::Regtest,
        &dir.join("ord"),
        &harness.core.url(),
        None,
        true,
        DEFAULT_ORD_CACHE_BYTES,
    )
    .unwrap();
    Index::open(&settings).unwrap()
}

/// The rune id a mined `txid` etches.
fn rune_id(harness: &Harness, txid: Txid) -> RuneId {
    let (block, tx) = harness.core.tx_index(txid);
    RuneId {
        block: block as u64,
        tx: u32::try_from(tx).unwrap(),
    }
}

fn height_of(harness: &Harness, txid: Txid) -> u32 {
    u32::try_from(harness.core.tx_index(txid).0).unwrap()
}

fn pkscript(harness: &Harness, outpoint: OutPoint) -> Vec<u8> {
    harness.core.tx_by_id(outpoint.txid).output[outpoint.vout as usize]
        .script_pubkey
        .to_bytes()
}

fn out(txid: Txid, vout: u32) -> OutPoint {
    OutPoint { txid, vout }
}

fn etch(premine: Option<u128>, terms: Option<Terms>) -> Runestone {
    Runestone {
        etching: Some(Etching {
            divisibility: Some(2),
            premine,
            rune: None,
            spacers: None,
            symbol: Some('$'),
            terms,
            turbo: true,
        }),
        ..Default::default()
    }
}

fn mintable(amount: u128) -> Option<Terms> {
    Some(Terms {
        amount: Some(amount),
        cap: Some(100),
        height: (None, None),
        offset: (None, None),
    })
}

fn bare(kind: RuneEventKind, txid: Txid, id: RuneId, amount: u128) -> RuneEvent {
    RuneEvent {
        kind,
        txid: txid.to_string(),
        outpoint: None,
        pkscript: None,
        rune_id: id.to_string(),
        amount,
    }
}

fn located(
    harness: &Harness,
    kind: RuneEventKind,
    txid: Txid,
    outpoint: OutPoint,
    id: RuneId,
    amount: u128,
) -> RuneEvent {
    RuneEvent {
        kind,
        txid: txid.to_string(),
        outpoint: Some(outpoint.to_string()),
        pkscript: Some(pkscript(harness, outpoint)),
        rune_id: id.to_string(),
        amount,
    }
}

fn allocation(txid: Txid, id: RuneId, amount: u128) -> RuneEvent {
    bare(RuneEventKind::NewAllocation, txid, id, amount)
}

fn mint(txid: Txid, id: RuneId, amount: u128) -> RuneEvent {
    bare(RuneEventKind::Mint, txid, id, amount)
}

fn burn(txid: Txid, id: RuneId, amount: u128) -> RuneEvent {
    bare(RuneEventKind::Burn, txid, id, amount)
}

fn output(harness: &Harness, outpoint: OutPoint, id: RuneId, amount: u128) -> RuneEvent {
    located(
        harness,
        RuneEventKind::Output,
        outpoint.txid,
        outpoint,
        id,
        amount,
    )
}

fn input(harness: &Harness, txid: Txid, spent: OutPoint, id: RuneId, amount: u128) -> RuneEvent {
    located(harness, RuneEventKind::Input, txid, spent, id, amount)
}

fn counter(id: RuneId, burned: u128, mints: u128) -> RuneCounter {
    RuneCounter {
        rune_id: id.to_string(),
        burned,
        mints,
    }
}

/// The rune rows of `reader`'s journal, table by table.
fn rune_rows(reader: &JournalReader) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    RUNE_TABLES
        .iter()
        .map(|name| segment_rows(reader.journal(), name))
        .collect()
}

/// Asserts the rune tables of `reader` equal those of a fresh source synced
/// to the harness's chain under its own directory.
fn assert_runes_match_fresh_run(harness: &Harness, reader: &JournalReader) {
    let tmp = tempfile::tempdir().unwrap();
    let (source, fresh) = synced(harness, tmp.path());
    stop(source);
    let expected = rune_rows(&fresh);
    assert!(expected.iter().any(|rows| !rows.is_empty()));
    assert_eq!(rune_rows(reader), expected);
}

#[test]
fn etch_with_premine_records_allocation_output_and_etching() {
    let harness = Harness::new();
    let funding = harness.fund();
    let txid = harness.runestone(&[funding], 1, etch(Some(1000), mintable(10)));
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let id = rune_id(&harness, txid);
    let height = height_of(&harness, txid);
    let view = view(&reader, height);
    assert_eq!(
        view.runes.events,
        [
            allocation(txid, id, 1000),
            output(&harness, out(txid, 0), id, 1000)
        ]
    );
    assert!(view.runes.counters.is_empty());
    stop(source);

    let index = ord_index(&harness, tmp.path());
    let rune = index.get_rune_by_id(id).unwrap().unwrap();
    let (_, entry, _) = index.rune(rune).unwrap().unwrap();
    let terms = entry.terms.unwrap();
    assert_eq!(
        view.runes.etchings,
        [RuneEtching {
            rune_id: id.to_string(),
            block: entry.block,
            divisibility: entry.divisibility,
            etching: entry.etching.to_string(),
            terms: Some(opi_core::RuneTerms {
                amount: terms.amount,
                cap: terms.cap,
                height: terms.height,
                offset: terms.offset,
            }),
            number: entry.number,
            premine: entry.premine,
            rune: entry.spaced_rune.rune.to_string(),
            spacers: entry.spaced_rune.spacers,
            symbol: entry.symbol,
            timestamp: u64::from(view.time),
            turbo: entry.turbo,
        }]
    );
    let etching = &view.runes.etchings[0];
    assert_eq!(
        (
            etching.premine,
            etching.divisibility,
            etching.symbol,
            etching.turbo
        ),
        (1000, 2, Some('$'), true)
    );
    assert_eq!(etching.etching, txid.to_string());
    assert_eq!(etching.terms.as_ref().unwrap().amount, Some(10));
}

#[test]
fn zero_premine_etching_still_records_allocation() {
    let harness = Harness::new();
    let funding = harness.fund();
    let txid = harness.runestone(&[funding], 1, etch(None, mintable(10)));
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let id = rune_id(&harness, txid);
    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(view.runes.events, [allocation(txid, id, 0)]);
    assert_eq!(view.runes.etchings.len(), 1);
    assert_eq!(view.runes.etchings[0].premine, 0);
    stop(source);
}

#[test]
fn cenotaph_etching_records_etching_and_burn_but_no_allocation() {
    let harness = Harness::new();
    let funding = harness.fund();
    let etched = harness.runestone(&[funding], 1, etch(Some(500), None));
    harness.mine(1);
    let held = rune_id(&harness, etched);

    let name = "AAAAAAAAAAAAAAAA".parse::<Rune>().unwrap();
    let (committed, witness) = harness.commit(name);
    // An edict to an output the transaction lacks makes the runestone a
    // cenotaph that still etches its committed rune.
    let cenotaph = Runestone {
        etching: Some(Etching {
            rune: Some(name),
            premine: Some(77),
            divisibility: Some(3),
            ..Default::default()
        }),
        edicts: vec![Edict {
            id: held,
            amount: 1,
            output: 9,
        }],
        ..Default::default()
    };
    let txid = harness.runestone_with_witness(
        &[
            (committed, witness),
            (out(etched, 0), bitcoin::Witness::new()),
        ],
        1,
        cenotaph,
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let id = rune_id(&harness, txid);
    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(
        view.runes.events,
        [
            input(&harness, txid, out(etched, 0), held, 500),
            burn(txid, held, 500)
        ]
    );
    assert_eq!(
        view.runes.etchings,
        [RuneEtching {
            rune_id: id.to_string(),
            block: id.block,
            divisibility: 0,
            etching: txid.to_string(),
            terms: None,
            number: 1,
            premine: 0,
            rune: name.to_string(),
            spacers: 0,
            symbol: None,
            timestamp: u64::from(view.time),
            turbo: false,
        }]
    );
    assert_eq!(view.runes.counters, [counter(held, 500, 0)]);
    assert!(journal_balances(&reader).is_empty());
    stop(source);
}

#[test]
fn mint_then_transfer_split_across_two_outputs() {
    let harness = Harness::new();
    let funding = harness.fund();
    let etched = harness.runestone(&[funding], 1, etch(None, mintable(100)));
    harness.mine(1);
    let id = rune_id(&harness, etched);

    let minter = harness.fund();
    let txid = harness.runestone(
        &[minter],
        2,
        Runestone {
            mint: Some(id),
            edicts: vec![Edict {
                id,
                amount: 40,
                output: 1,
            }],
            ..Default::default()
        },
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(
        view.runes.events,
        [
            mint(txid, id, 100),
            output(&harness, out(txid, 0), id, 60),
            output(&harness, out(txid, 1), id, 40),
        ]
    );
    assert_eq!(view.runes.counters, [counter(id, 0, 1)]);
    stop(source);
}

#[test]
fn spend_of_two_rune_outpoints_orders_inputs_by_input_order() {
    let harness = Harness::new();
    let funding = harness.fund();
    let mut split = etch(Some(1000), None);
    split.edicts = vec![Edict {
        id: RuneId::default(),
        amount: 400,
        output: 1,
    }];
    let etched = harness.runestone(&[funding], 2, split);
    harness.mine(1);
    let id = rune_id(&harness, etched);

    // Inputs in descending outpoint order.
    let txid = harness.runestone(&[out(etched, 1), out(etched, 0)], 1, Runestone::default());
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(
        view.runes.events,
        [
            input(&harness, txid, out(etched, 1), id, 400),
            input(&harness, txid, out(etched, 0), id, 600),
            output(&harness, out(txid, 0), id, 1000),
        ]
    );
    assert_eq!(
        journal_balances(&reader).into_keys().collect::<Vec<_>>(),
        [out(txid, 0)]
    );
    stop(source);
}

#[test]
fn two_burns_are_ordered_by_rune_id() {
    // Six runes, so ord's random burn order is rarely sorted by chance.
    const AMOUNTS: [u128; 6] = [100, 200, 300, 400, 500, 600];
    let harness = Harness::new();
    let fundings = AMOUNTS.map(|_| harness.fund());
    let etched = fundings
        .iter()
        .zip(AMOUNTS)
        .map(|(funding, premine)| harness.runestone(&[*funding], 1, etch(Some(premine), None)))
        .collect::<Vec<_>>();
    harness.mine(1);
    let ids = etched
        .iter()
        .map(|txid| rune_id(&harness, *txid))
        .collect::<Vec<_>>();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));

    // Inputs in descending rune id order; every edict sends its rune to the
    // OP_RETURN output, burning it.
    let spent = etched
        .iter()
        .rev()
        .map(|txid| out(*txid, 0))
        .collect::<Vec<_>>();
    let txid = harness.runestone(
        &spent,
        1,
        Runestone {
            edicts: ids
                .iter()
                .rev()
                .map(|id| Edict {
                    id: *id,
                    amount: 0,
                    output: 1,
                })
                .collect(),
            ..Default::default()
        },
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let view = view(&reader, height_of(&harness, txid));
    let mut expected = etched
        .iter()
        .zip(&ids)
        .zip(AMOUNTS)
        .rev()
        .map(|((etched, id), amount)| input(&harness, txid, out(*etched, 0), *id, amount))
        .collect::<Vec<_>>();
    expected.extend(
        ids.iter()
            .zip(AMOUNTS)
            .map(|(id, amount)| burn(txid, *id, amount)),
    );
    assert_eq!(view.runes.events, expected);
    assert_eq!(
        view.runes.counters,
        ids.iter()
            .zip(AMOUNTS)
            .map(|(id, amount)| counter(*id, amount, 0))
            .collect::<Vec<_>>()
    );
    stop(source);
}

#[test]
fn spend_within_the_same_block_finds_the_new_output() {
    let harness = Harness::new();
    let funding = harness.fund();
    let etched = harness.runestone(&[funding], 1, etch(Some(1000), None));
    let txid = harness.runestone(&[out(etched, 0)], 1, Runestone::default());
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let id = rune_id(&harness, etched);
    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(
        view.runes.events,
        [
            allocation(etched, id, 1000),
            output(&harness, out(etched, 0), id, 1000),
            input(&harness, txid, out(etched, 0), id, 1000),
            output(&harness, out(txid, 0), id, 1000),
        ]
    );
    assert_eq!(
        journal_balances(&reader),
        BTreeMap::from([(
            out(txid, 0),
            (
                pkscript(&harness, out(txid, 0)),
                vec![(id.to_string(), 1000)]
            )
        )])
    );
    stop(source);
}

#[test]
fn runes_only_block_reads_full_block() {
    let harness = Harness::new();
    let funding = harness.fund();
    let txid = harness.runestone(&[funding], 1, etch(Some(5), None));
    harness.mine(1);
    // Enough later blocks that the rune block leaves the proxy's block cache
    // before finalize reads it.
    harness.mine(40);
    let tmp = tempfile::tempdir().unwrap();
    let source = spawn_source(source_config(tmp.path(), harness.core.url(), 100)).unwrap();
    let reader = source.reader();
    wait_for_tip(&reader, harness.height());

    let id = rune_id(&harness, txid);
    let view = view(&reader, height_of(&harness, txid));
    assert!(view.inscriptions.records.is_empty());
    assert_eq!(
        view.runes.events,
        [
            allocation(txid, id, 5),
            output(&harness, out(txid, 0), id, 5)
        ]
    );
    stop(source);
}

#[test]
fn inscription_and_rune_in_one_transaction_fill_both_feeds() {
    let harness = Harness::new();
    let funding = harness.fund();
    let txid = harness.runestone_with_witness(
        &[(funding, envelope(b"text/plain", JSON))],
        1,
        etch(Some(7), None),
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    let id = rune_id(&harness, txid);
    let view = view(&reader, height_of(&harness, txid));
    assert_eq!(view.inscriptions.records.len(), 1);
    assert_eq!(
        view.inscriptions.records[0].inscription_id,
        format!("{txid}i0")
    );
    assert_eq!(
        view.runes.events,
        [
            allocation(txid, id, 7),
            output(&harness, out(txid, 0), id, 7)
        ]
    );
    assert_eq!(view.runes.etchings.len(), 1);
    stop(source);
}

#[test]
fn counters_accumulate_across_blocks() {
    let harness = Harness::new();
    let funding = harness.fund();
    let etched = harness.runestone(&[funding], 1, etch(None, mintable(10)));
    harness.mine(1);
    let id = rune_id(&harness, etched);

    let first = harness.fund();
    let first = harness.runestone(
        &[first],
        1,
        Runestone {
            mint: Some(id),
            ..Default::default()
        },
    );
    harness.mine(1);
    // The second mint burns 2 of its 10 through an edict to the OP_RETURN.
    let second = harness.fund();
    let second = harness.runestone(
        &[second],
        1,
        Runestone {
            mint: Some(id),
            edicts: vec![Edict {
                id,
                amount: 2,
                output: 1,
            }],
            ..Default::default()
        },
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());

    assert!(
        view(&reader, height_of(&harness, etched))
            .runes
            .counters
            .is_empty()
    );
    assert_eq!(
        view(&reader, height_of(&harness, first)).runes.counters,
        [counter(id, 0, 1)]
    );
    assert_eq!(
        view(&reader, height_of(&harness, second)).runes.counters,
        [counter(id, 2, 2)]
    );
    stop(source);
}

#[test]
fn balances_match_ord_at_tip() {
    let harness = Harness::new();
    let mut split = etch(Some(1000), mintable(50));
    split.edicts = vec![Edict {
        id: RuneId::default(),
        amount: 300,
        output: 1,
    }];
    let fundings = [harness.fund(), harness.fund(), harness.fund()];
    let a = harness.runestone(&[fundings[0]], 2, split);
    let b = harness.runestone(&[fundings[1]], 1, etch(Some(70), None));
    harness.mine(1);
    let (a_id, b_id) = (rune_id(&harness, a), rune_id(&harness, b));

    // Merge one output of each rune, then spend the merge in the same block.
    let merged = harness.runestone(&[out(a, 1), out(b, 0)], 2, Runestone::default());
    let respent = harness.runestone(
        &[out(merged, 0)],
        2,
        Runestone {
            edicts: vec![Edict {
                id: b_id,
                amount: 20,
                output: 1,
            }],
            ..Default::default()
        },
    );
    harness.runestone(
        &[fundings[2]],
        1,
        Runestone {
            mint: Some(a_id),
            ..Default::default()
        },
    );
    harness.mine(1);
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    stop(source);

    let journal = journal_balances(&reader);
    assert!(journal.contains_key(&out(respent, 1)));
    let index = ord_index(&harness, tmp.path());
    let ord = index.get_rune_balances().unwrap();
    assert_eq!(journal.len(), ord.len());
    for (outpoint, balances) in ord {
        let (stored_pkscript, stored) = &journal[&outpoint];
        assert_eq!(*stored_pkscript, pkscript(&harness, outpoint));
        assert_eq!(
            *stored,
            balances
                .iter()
                .map(|(id, amount)| (id.to_string(), *amount))
                .collect::<Vec<_>>()
        );
        let by_rune = index
            .get_rune_balances_for_output(outpoint)
            .unwrap()
            .unwrap();
        assert_eq!(by_rune.len(), stored.len());
    }
}

#[test]
fn reorg_restores_spent_rune_outpoints() {
    let harness = Harness::new();
    let fundings = [harness.fund(), harness.fund(), harness.fund()];
    let a = harness.runestone(&[fundings[0]], 1, etch(Some(1000), None));
    let b = harness.runestone(&[fundings[1]], 1, etch(Some(70), None));
    harness.mine(1);
    let (a_id, b_id) = (rune_id(&harness, a), rune_id(&harness, b));
    // One outpoint holding both runes.
    let merged = harness.runestone(&[out(a, 0), out(b, 0)], 1, Runestone::default());
    harness.mine(1);

    // The tip spends the merge, spends that spend's output in the same block
    // and etches a third rune.
    let split = harness.runestone(
        &[out(merged, 0)],
        2,
        Runestone {
            edicts: vec![Edict {
                id: b_id,
                amount: 20,
                output: 1,
            }],
            ..Default::default()
        },
    );
    let respent = harness.runestone(&[out(split, 0)], 1, Runestone::default());
    let c = harness.runestone(&[fundings[2]], 1, etch(Some(5), None));
    harness.mine(1);
    let tip = harness.height();
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    let mut created = vec![out(split, 1), out(respent, 0), out(c, 0)];
    created.sort();
    assert_eq!(
        journal_balances(&reader).into_keys().collect::<Vec<_>>(),
        created
    );

    harness.invalidate_tip();
    harness.mine(2);
    wait_for_tip(&reader, tip + 1);
    stop(source);

    assert_eq!(
        journal_balances(&reader),
        BTreeMap::from([(
            out(merged, 0),
            (
                pkscript(&harness, out(merged, 0)),
                vec![(a_id.to_string(), 1000), (b_id.to_string(), 70)]
            )
        )])
    );
    assert!(view(&reader, tip).runes.events.is_empty());
    assert!(view(&reader, tip).runes.etchings.is_empty());
    assert_runes_match_fresh_run(&harness, &reader);
}

#[test]
fn reorg_restores_both_outpoints_one_transaction_spends() {
    let harness = Harness::new();
    let funding = harness.fund();
    let mut split = etch(Some(1000), None);
    split.edicts = vec![Edict {
        id: RuneId::default(),
        amount: 400,
        output: 1,
    }];
    let etched = harness.runestone(&[funding], 2, split);
    harness.mine(1);
    let id = rune_id(&harness, etched);

    // The tip spends both rune outpoints in one transaction.
    let txid = harness.runestone(&[out(etched, 1), out(etched, 0)], 1, Runestone::default());
    harness.mine(1);
    let tip = harness.height();
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    assert_eq!(
        journal_balances(&reader).into_keys().collect::<Vec<_>>(),
        [out(txid, 0)]
    );

    harness.invalidate_tip();
    harness.mine(2);
    wait_for_tip(&reader, tip + 1);
    stop(source);

    assert_eq!(
        journal_balances(&reader),
        BTreeMap::from([
            (
                out(etched, 0),
                (
                    pkscript(&harness, out(etched, 0)),
                    vec![(id.to_string(), 600)]
                )
            ),
            (
                out(etched, 1),
                (
                    pkscript(&harness, out(etched, 1)),
                    vec![(id.to_string(), 400)]
                )
            ),
        ])
    );
    assert!(view(&reader, tip).runes.events.is_empty());
    assert_runes_match_fresh_run(&harness, &reader);
}

#[test]
fn reorg_restores_counters() {
    let harness = Harness::new();
    let fundings = [harness.fund(), harness.fund()];
    let x = harness.runestone(&[fundings[0]], 1, etch(None, mintable(10)));
    let y = harness.runestone(&[fundings[1]], 1, etch(None, mintable(10)));
    harness.mine(1);
    let (x_id, y_id) = (rune_id(&harness, x), rune_id(&harness, y));

    let minter = harness.fund();
    harness.runestone(
        &[minter],
        1,
        Runestone {
            mint: Some(x_id),
            ..Default::default()
        },
    );
    harness.mine(1);
    let before = journal_counters_after(&harness);

    // The tip mints both runes; the mint of the never-minted rune burns 2.
    let minters = [harness.fund(), harness.fund()];
    harness.runestone(
        &[minters[0]],
        1,
        Runestone {
            mint: Some(x_id),
            ..Default::default()
        },
    );
    harness.runestone(
        &[minters[1]],
        1,
        Runestone {
            mint: Some(y_id),
            edicts: vec![Edict {
                id: y_id,
                amount: 2,
                output: 1,
            }],
            ..Default::default()
        },
    );
    harness.mine(1);
    let tip = harness.height();
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    assert_eq!(
        journal_counters(&reader),
        BTreeMap::from([(x_id.to_string(), (0, 2)), (y_id.to_string(), (2, 1))])
    );

    harness.invalidate_tip();
    harness.mine(2);
    wait_for_tip(&reader, tip + 1);
    stop(source);

    assert_eq!(
        before,
        BTreeMap::from([(x_id.to_string(), (0, 1))]),
        "the counters before the tip"
    );
    assert_eq!(journal_counters(&reader), before);
    assert!(view(&reader, tip).runes.counters.is_empty());
    assert_runes_match_fresh_run(&harness, &reader);
}

/// The `rune_counters` of a fresh source synced to the harness's chain.
fn journal_counters_after(harness: &Harness) -> BTreeMap<String, RuneTotals> {
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(harness, tmp.path());
    stop(source);
    journal_counters(&reader)
}

#[test]
fn reorg_of_two_blocks_drops_outpoints_created_and_spent_above_it() {
    let harness = Harness::new();
    // Below ord's savepoint interval (10) ord saves every block and keeps
    // two savepoints, too few to roll back two blocks.
    harness.mine(20);
    let funding = harness.fund();
    let etched = harness.runestone(&[funding], 1, etch(Some(300), None));
    harness.mine(1);
    let id = rune_id(&harness, etched);
    let fork = harness.height();

    let moved = harness.runestone(&[out(etched, 0)], 1, Runestone::default());
    harness.mine(1);
    let burned = harness.runestone(
        &[out(moved, 0)],
        1,
        Runestone {
            edicts: vec![Edict {
                id,
                amount: 100,
                output: 1,
            }],
            ..Default::default()
        },
    );
    harness.mine(1);
    let tip = harness.height();
    let tmp = tempfile::tempdir().unwrap();
    let (source, reader) = synced(&harness, tmp.path());
    assert_eq!(
        journal_balances(&reader).into_keys().collect::<Vec<_>>(),
        [out(burned, 0)]
    );
    assert_eq!(journal_counters(&reader).len(), 1);

    harness.invalidate_tip();
    harness.invalidate_tip();
    assert_eq!(harness.height(), fork);
    harness.mine(3);
    wait_for_tip(&reader, tip + 1);
    stop(source);

    assert_eq!(
        journal_balances(&reader),
        BTreeMap::from([(
            out(etched, 0),
            (
                pkscript(&harness, out(etched, 0)),
                vec![(id.to_string(), 300)]
            )
        )])
    );
    assert!(journal_counters(&reader).is_empty());
    assert_runes_match_fresh_run(&harness, &reader);
}
