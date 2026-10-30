//! Reproduction of the journal file growth seen on mainnet at height 824k,
//! on the single-file layout's table shape: a compacted journal, pruned by
//! one long write transaction per 1000 heights (a5c0eff) or by short steps
//! of 10,000 rows (cc93c22), with and without a reader holding a snapshot,
//! then written to as ord's indexing does.
//!
//! The harness writes the old layout's tables with redb directly, so it
//! measures the transaction shapes without the old code. Ignored by
//! default; it writes several GB. Run it in release:
//!
//! ```text
//! cargo test --release -p opi-source --test incident -- --ignored --nocapture
//! ```
//!
//! `INCIDENT_HEIGHTS`, `INCIDENT_CREATIONS`, `INCIDENT_TRANSFERS`,
//! `INCIDENT_RAW` and `INCIDENT_CONTENT` set the shape of the journal, and
//! `INCIDENT_DIR` where it is written.

use {
    redb::{
        Database, Durability, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction,
    },
    std::{path::Path, time::Instant},
};

const RAW_EVENTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("raw_events");
const RAW_HEIGHTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("raw_heights");
const TRANSFERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("ord_transfers");
const INFO: TableDefinition<&[u8], &[u8]> = TableDefinition::new("ord_inscription_info");

/// cc93c22's row budget per prune transaction.
const STEP_ROWS: usize = 10_000;

fn env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 30)
}

fn key(height: u32, seq: u32) -> [u8; 8] {
    let mut key = [0; 8];
    key[..4].copy_from_slice(&height.to_be_bytes());
    key[4..].copy_from_slice(&seq.to_be_bytes());
    key
}

/// The info key of the `index`th inscription created at `height`: a txid
/// stored in internal byte order, so a height's keys scatter over the
/// table as real txids do.
fn info_key(height: u32, index: u32) -> [u8; 36] {
    let mut id = [0; 36];
    let seed = (u64::from(height) << 32 | u64::from(index)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for (i, chunk) in id[..32].chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&seed.rotate_left(i as u32 * 13).to_le_bytes());
    }
    id
}

fn begin_write(db: &Database) -> WriteTransaction {
    let mut tx = db.begin_write().unwrap();
    tx.set_durability(Durability::Immediate).unwrap();
    tx.set_quick_repair(true);
    tx
}

#[derive(Debug, Clone, Copy)]
struct Shape {
    heights: u32,
    raw: u32,
    creations: u32,
    transfers: u32,
    content: usize,
}

impl Shape {
    fn from_env() -> Shape {
        Shape {
            heights: env("INCIDENT_HEIGHTS", 2000),
            raw: env("INCIDENT_RAW", 6000),
            creations: env("INCIDENT_CREATIONS", 3000),
            transfers: env("INCIDENT_TRANSFERS", 1500),
            content: env("INCIDENT_CONTENT", 120) as usize,
        }
    }

    /// Writes `heights` as finalize and the stager did, one durable
    /// transaction per height: raw events and their tag, a record and an info
    /// row per creation, and a record per first transfer of an inscription
    /// created at the height before.
    fn fill(&self, db: &Database, heights: std::ops::Range<u32>) {
        self.fill_with(db, heights, 1);
    }

    /// [`Shape::fill`] with `batch` heights per transaction and only the
    /// last durable, to build the starting journal quickly. Its layout is
    /// then rewritten by a compaction.
    fn fill_with(&self, db: &Database, heights: std::ops::Range<u32>, batch: u32) {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let last = heights.end.saturating_sub(1);
        for start in heights.clone().step_by(batch as usize) {
            let mut tx = db.begin_write().unwrap();
            let end = (start + batch).min(heights.end);
            if batch > 1 && end <= last {
                tx.set_durability(Durability::None).unwrap();
            } else {
                tx.set_durability(Durability::Immediate).unwrap();
                tx.set_quick_repair(true);
            }
            for height in start..end {
                self.write_height(&tx, height, &mut seed);
            }
            tx.commit().unwrap();
        }
    }

    fn write_height(&self, tx: &WriteTransaction, height: u32, seed: &mut u64) {
        let mut events = tx.open_table(RAW_EVENTS).unwrap();
        for seq in 0..self.raw {
            events
                .insert(key(height, seq).as_slice(), [7u8; 120].as_slice())
                .unwrap();
        }
        tx.open_table(RAW_HEIGHTS)
            .unwrap()
            .insert(height.to_be_bytes().as_slice(), [0u8; 32].as_slice())
            .unwrap();
        let mut transfers = tx.open_table(TRANSFERS).unwrap();
        let mut infos = tx.open_table(INFO).unwrap();
        let mut idx = 0;
        for index in 0..self.creations {
            let mut record = [0u8; 160];
            record[..36].copy_from_slice(&info_key(height, index));
            transfers
                .insert(key(height, idx).as_slice(), record.as_slice())
                .unwrap();
            idx += 1;
            let content = (0..self.content)
                .map(|_| {
                    *seed ^= *seed << 13;
                    *seed ^= *seed >> 7;
                    *seed ^= *seed << 17;
                    *seed as u8
                })
                .collect::<Vec<_>>();
            infos
                .insert(info_key(height, index).as_slice(), content.as_slice())
                .unwrap();
        }
        if height > 0 {
            for index in 0..self.transfers.min(self.creations) {
                let mut record = [1u8; 160];
                record[..36].copy_from_slice(&info_key(height - 1, index));
                transfers
                    .insert(key(height, idx).as_slice(), record.as_slice())
                    .unwrap();
                idx += 1;
            }
        }
    }
}

/// The infos a range of transfer records consumes: those of first
/// transfers, whose record value starts with the inscription id and whose
/// filler byte is 1.
fn consumed(record: &[u8]) -> Option<Vec<u8>> {
    (record[36] == 1).then(|| record[..36].to_vec())
}

/// a5c0eff's prune of `from..to`: raw events and tags in one transaction,
/// then transfer records with the infos their first transfers consume in a
/// second one. `report` runs after the first.
fn prune_long(db: &Database, from: u32, to: u32, report: impl Fn(&str)) {
    let tx = begin_write(db);
    {
        let (first, end) = (key(from, 0), key(to, 0));
        tx.open_table(RAW_EVENTS)
            .unwrap()
            .retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)
            .unwrap();
        let (first, end) = (from.to_be_bytes(), to.to_be_bytes());
        tx.open_table(RAW_HEIGHTS)
            .unwrap()
            .retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)
            .unwrap();
    }
    tx.commit().unwrap();
    report(&format!("  a5c0eff raw events below {to}"));

    let tx = begin_write(db);
    {
        let mut transfers = tx.open_table(TRANSFERS).unwrap();
        let mut infos = tx.open_table(INFO).unwrap();
        let (first, end) = (key(from, 0), key(to, 0));
        let ids = transfers
            .range::<&[u8]>(first.as_slice()..end.as_slice())
            .unwrap()
            .filter_map(|row| consumed(row.unwrap().1.value()))
            .collect::<Vec<_>>();
        for id in ids {
            infos.remove(id.as_slice()).unwrap();
        }
        transfers
            .retain_in::<&[u8], _>(first.as_slice()..end.as_slice(), |_, _| false)
            .unwrap();
    }
    tx.commit().unwrap();
}

/// cc93c22's prune below `below`: each step is one transaction deleting at
/// most [`STEP_ROWS`] raw events and one deleting at most [`STEP_ROWS`]
/// transfer records and consumed infos together. Returns the steps it took.
fn prune_short(db: &Database, below: u32) -> u32 {
    let end = key(below, 0);
    let mut steps = 0;
    loop {
        steps += 1;
        let tx = begin_write(db);
        let raw_done = {
            let mut events = tx.open_table(RAW_EVENTS).unwrap();
            let cut = events
                .range::<&[u8]>(..end.as_slice())
                .unwrap()
                .nth(STEP_ROWS)
                .map(|row| row.unwrap().0.value().to_vec());
            match &cut {
                Some(cut) => events.retain_in::<&[u8], _>(..cut.as_slice(), |_, _| false),
                None => events.retain_in::<&[u8], _>(..end.as_slice(), |_, _| false),
            }
            .unwrap();
            cut.is_none()
        };
        tx.commit().unwrap();

        let tx = begin_write(db);
        let records_done = {
            let mut transfers = tx.open_table(TRANSFERS).unwrap();
            let mut infos = tx.open_table(INFO).unwrap();
            let mut taken = 0;
            let mut cut = None;
            let mut ids = Vec::new();
            for row in transfers.range::<&[u8]>(..end.as_slice()).unwrap() {
                let (key, value) = row.unwrap();
                let id = consumed(value.value());
                let cost = 1 + usize::from(id.is_some());
                if taken > 0 && taken + cost > STEP_ROWS {
                    cut = Some(key.value().to_vec());
                    break;
                }
                taken += cost;
                ids.extend(id);
            }
            match &cut {
                Some(cut) => transfers.retain_in::<&[u8], _>(..cut.as_slice(), |_, _| false),
                None => transfers.retain_in::<&[u8], _>(..end.as_slice(), |_, _| false),
            }
            .unwrap();
            for id in ids {
                infos.remove(id.as_slice()).unwrap();
            }
            cut.is_none()
        };
        tx.commit().unwrap();
        if raw_done && records_done {
            return steps;
        }
    }
}

struct Run<'a> {
    shape: Shape,
    path: &'a Path,
    db: Database,
}

impl Run<'_> {
    fn stored(&self) -> u64 {
        let tx = self.db.begin_write().unwrap();
        let stats = tx.stats().unwrap();
        tx.abort().unwrap();
        stats.stored_bytes() + stats.metadata_bytes()
    }

    fn report(&self, stage: &str) {
        let file = std::fs::metadata(self.path).unwrap().len();
        let stored = self.stored();
        println!(
            "{stage:<44} file {:>7.2} GiB  stored {:>7.2} GiB  file/stored {:>5.2}",
            gib(file),
            gib(stored),
            file as f64 / stored as f64
        );
    }
}

/// Fills a journal at `path`, compacts it so it has no free pages, prunes
/// half of it with `prune`, a reader open throughout when `reader` is set,
/// then indexes 200 heights with the reader still open and 200 after it
/// closes.
fn scenario(shape: Shape, path: &Path, reader: bool, long: bool) {
    let started = Instant::now();
    let mut db = Database::builder().create(path).unwrap();
    shape.fill_with(&db, 0..shape.heights, 50);
    while db.compact().unwrap() {}
    let run = Run { shape, path, db };
    println!("  filled and compacted in {:.0?}", started.elapsed());
    run.report("  compacted");

    let snapshot = reader.then(|| run.db.begin_read().unwrap());
    let started = Instant::now();
    let below = shape.heights / 2;
    if long {
        for from in (0..below).step_by(1000) {
            let to = (from + 1000).min(below);
            prune_long(&run.db, from, to, |stage| run.report(stage));
            run.report(&format!("  a5c0eff records and infos below {to}"));
        }
    } else {
        let steps = prune_short(&run.db, below);
        run.report(&format!("  cc93c22 prune below {below}, {steps} steps"));
    }
    println!("  pruned in {:.0?}", started.elapsed());
    run.shape.fill(&run.db, shape.heights..shape.heights + 200);
    run.report("  then indexed 200 heights");
    drop(snapshot);
    run.shape
        .fill(&run.db, shape.heights + 200..shape.heights + 400);
    run.report("  reader closed, indexed 200 more");
    drop(run);
    std::fs::remove_file(path).unwrap();
}

#[test]
#[ignore = "writes several GB; run in release with --ignored"]
fn compacted_journal_grows_under_a_long_prune() {
    let shape = Shape::from_env();
    println!("{shape:?}");
    let dir = tempfile::tempdir_in(std::env::var("INCIDENT_DIR").unwrap_or(".".into())).unwrap();
    for reader in [false, true] {
        for long in [true, false] {
            let name = if long { "a5c0eff" } else { "cc93c22" };
            println!("\n== {name}, reader open: {reader}");
            scenario(
                shape,
                &dir.path().join(format!("{name}-{reader}.redb")),
                reader,
                long,
            );
        }
    }
}
