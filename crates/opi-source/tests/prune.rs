mod support;

use {
    bitcoin::{BlockHash, OutPoint, hashes::Hash},
    opi_core::BlockSource,
    opi_source::{
        driver::{SourceConfig, spawn_source},
        journal::{
            HeightFile, Journal, JournalOptions, JournalReader, Stream, cf, raw_event_key,
            rollback_to,
            testing::{self, Density, KillPoint},
        },
    },
    ord::{Chain, InscriptionId},
    std::{
        collections::{BTreeMap, BTreeSet},
        os::unix::fs::MetadataExt,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    },
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

/// Rows of table `name` below `height` across the journal's files.
fn rows_below(journal: &Journal, name: &'static str, height: u32) -> usize {
    segment_rows(journal, name)
        .iter()
        .filter(|(key, _)| u32::from_be_bytes(key[..4].try_into().unwrap()) < height)
        .count()
}

/// The directory of the record segment starting at `start`.
fn record_segment_dir(journal: &Journal, start: u32) -> std::path::PathBuf {
    journal
        .dir()
        .join(Stream::Records.dir_name())
        .join(format!("{start:09}"))
}

/// Pruning deletes the record segments below both bounds, the reader refuses
/// their heights, and the tracked set keeps what later blocks need: an
/// inscription not yet transferred keeps its row, so its first transfer
/// after the prune is still recorded with its content. Finalize leaves no
/// raw file behind.
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
    assert_eq!(journal.heights(Stream::Raw).unwrap(), Vec::<u32>::new());

    let floor = moved_at + 1;
    let segments = journal.segment_starts().unwrap().len();
    let pruned = journal.prune(floor, floor).unwrap();
    let below = floor - floor % 2;
    assert_eq!(pruned.segments, u64::from(below / 2), "{pruned:?}");
    assert_eq!(pruned.tracked, 1, "{pruned:?}");
    assert_eq!(
        journal.segment_starts().unwrap().len(),
        segments - below as usize / 2
    );
    assert_eq!(journal.pruned_below().unwrap(), below);
    for start in (0..below).step_by(2) {
        assert!(!record_segment_dir(&journal, start).exists());
    }
    for name in [cf::TRANSFERS, cf::HEADERS] {
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
/// height at or above either bound, and deletes the raw files below the raw
/// bound one by one.
#[test]
fn prune_deletes_whole_segments_below_both_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..450, SMALL).unwrap();
    assert_eq!(journal.segment_starts().unwrap(), [0, 100, 200, 300, 400]);

    let pruned = journal.prune(250, 150).unwrap();
    assert_eq!((pruned.segments, pruned.raw), (1, 250));
    assert_eq!(journal.segment_starts().unwrap(), [100, 200, 300, 400]);
    assert_eq!(journal.pruned_below().unwrap(), 100);
    assert_eq!(journal.heights(Stream::Raw).unwrap()[0], 250);

    assert_eq!(journal.prune(450, 450).unwrap().segments, 3);
    assert_eq!(journal.segment_starts().unwrap(), [400]);
    assert_eq!(journal.final_tip().unwrap(), Some(449));
}

/// A reader with a record file open keeps reading it after a prune deletes
/// it.
#[test]
fn an_open_file_outlives_its_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..250, SMALL).unwrap();
    let path = journal.height_path(Stream::Records, 50);
    let expected = std::fs::read(&path).unwrap();
    let mut held = std::fs::File::open(&path).unwrap();
    journal.prune(250, 250).unwrap();
    assert!(!path.exists());
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut read).unwrap();
    assert_eq!(read, expected);
}

/// A rollback across a segment boundary deletes the records above the
/// agreed height in both segments and lowers the final tip, and finalizing
/// on top of it works as before.
#[test]
fn rollback_crosses_a_segment_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open_with(tmp.path(), Chain::Regtest, false, &options(100)).unwrap();
    testing::fill(&journal, 0..204, SMALL).unwrap();
    rollback_to(&journal, 198).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(198));
    assert_eq!(journal.segment_starts().unwrap(), [0, 100]);
    for name in [cf::TRANSFERS, cf::INFO, cf::HEADERS] {
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

/// The raw file [`Journal::write_raw`] writes for `height`.
fn raw_file(height: u32, density: Density) -> HeightFile {
    let mut file = HeightFile::new(height, BlockHash::all_zeros().as_byte_array().to_vec());
    for (seq, event) in testing::raw_events(height, density).iter().enumerate() {
        file.rows_mut(cf::RAW_EVENTS)
            .insert(&raw_event_key(height, seq as u32), event)
            .unwrap();
    }
    file
}

/// Height files by (stream dir name, height).
type FileMap<T> = BTreeMap<(&'static str, u32), T>;

/// Every height file under `dir`: its length and allocated bytes, and every
/// `.tmp` file left.
fn files_on_disk(dir: &Path) -> (FileMap<(u64, u64)>, Vec<String>) {
    let mut files = BTreeMap::new();
    let mut tmp = Vec::new();
    for stream in Stream::ALL {
        let Ok(segments) = std::fs::read_dir(dir.join(stream.dir_name())) else {
            continue;
        };
        for segment in segments {
            for entry in std::fs::read_dir(segment.unwrap().path()).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                let meta = entry.metadata().unwrap();
                match name.parse::<u32>() {
                    Ok(height) => {
                        files.insert(
                            (stream.dir_name(), height),
                            (meta.len(), meta.blocks() * 512),
                        );
                    }
                    Err(_) => tmp.push(name),
                }
            }
        }
    }
    (files, tmp)
}

/// Reads block views of random heights between the pruned floor and the
/// final tip until `stop`, checking each against [`testing::records_file`]'s
/// shape. A height the journal no longer serves fails with the pruned floor
/// or reads as `None` after a rollback.
fn read_until(
    reader: JournalReader,
    density: Density,
    stop: Arc<AtomicBool>,
    reads: Arc<AtomicU64>,
) {
    let mut seed = 0x1234_5678u32;
    while !stop.load(Ordering::SeqCst) {
        let Some(tip) = reader.tip().unwrap() else {
            continue;
        };
        let floor = reader.journal().pruned_below().unwrap();
        if floor > tip {
            continue;
        }
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let height = floor + seed % (tip + 1 - floor);
        match reader.block_view(height) {
            Ok(Some(view)) => {
                let transfers = if height == 0 { 0 } else { density.transfers };
                assert_eq!(view.height, height);
                assert_eq!(
                    view.inscriptions.records.len() as u32,
                    density.creations + transfers
                );
                assert!(
                    view.inscriptions
                        .infos
                        .values()
                        .all(|info| info.content.len() == density.content)
                );
                reads.fetch_add(1, Ordering::SeqCst);
            }
            Ok(None) => {}
            Err(err) => assert!(format!("{err:#}").contains("pruned floor"), "{err:#}"),
        }
    }
}

/// The journal of the property test, with the model of what it must hold.
struct Model {
    density: Density,
    segment: u32,
    /// Bytes each height file must take: (stream, height) to length.
    files: FileMap<u64>,
    /// Highest height with raw events written, ahead of the final tip.
    staged: Option<u32>,
}

impl Model {
    fn stage_through(&mut self, journal: &Journal, height: u32) {
        let first = self.staged.map_or(0, |staged| staged + 1);
        for height in first..=height {
            journal
                .write_raw(
                    height,
                    BlockHash::all_zeros(),
                    &testing::raw_events(height, self.density),
                )
                .unwrap();
            let len = testing::encoded_len(&raw_file(height, self.density), Stream::Raw).unwrap();
            self.files.insert(("raw", height), len);
        }
        self.staged = self.staged.max(Some(height));
    }

    /// Finalizes `height`: its records, then the deletion of its raw file.
    fn finalize(&mut self, journal: &Journal, height: u32) {
        let file = testing::records_file(height, self.density).unwrap();
        journal.append_records(&file).unwrap();
        self.files.insert(
            ("rec", height),
            testing::encoded_len(&file, Stream::Records).unwrap(),
        );
        std::fs::remove_file(journal.height_path(Stream::Raw, height)).unwrap();
        self.files.remove(&("raw", height));
    }

    /// Checks the files on disk against the model: the same files, each its
    /// encoded length and allocated within one file system block of it, and
    /// no more record segments than the window allows.
    fn check(&self, journal: &Journal, keep: u32, context: &str) {
        let (files, tmp) = files_on_disk(journal.dir());
        assert_eq!(tmp, Vec::<String>::new(), "{context}");
        let lengths = files
            .iter()
            .map(|(key, (len, _))| (*key, *len))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(lengths, self.files, "{context}");
        let logical = files.values().map(|(len, _)| len).sum::<u64>();
        let allocated = files.values().map(|(_, allocated)| allocated).sum::<u64>();
        assert!(
            allocated <= logical + 4096 * files.len() as u64,
            "{context}: {allocated} bytes allocated for {logical} bytes in {} files",
            files.len()
        );
        let segments = journal.segment_starts().unwrap();
        assert!(
            segments.len() as u32 <= keep / self.segment + 2,
            "{context}: segments {segments:?}"
        );
        assert_eq!(
            journal.final_tip().unwrap(),
            self.files
                .keys()
                .filter(|(s, _)| *s == "rec")
                .map(|(_, h)| *h)
                .max(),
            "{context}"
        );
    }
}

/// Indexes several prune windows of blocks the size of 2025 mainnet blocks
/// (16 MiB of incompressible records per height) into a journal pruned to a
/// window of `keep` heights, with a reader reading every height it serves
/// throughout, reorgs across segment boundaries, and a kill at every step of
/// a height file's write in both streams, each followed by a restart. After
/// every step the journal holds exactly the files the model says, each
/// taking its encoded length, so its disk use is its data; and it never
/// holds more than the window's record segments plus two.
///
/// `OPI_PRUNE_PROPERTY_SCALE` multiplies the block size, for a heavier run
/// than CI's.
#[test]
fn disk_use_equals_data_across_windows() {
    let scale = env("OPI_PRUNE_PROPERTY_SCALE", 1);
    let density = Density {
        raw_events: 2000,
        creations: 96 * scale,
        transfers: 32 * scale,
        content: 128 << 10,
    };
    let (segment, keep, windows, ahead) = (5, 15, 4, 3);
    let kills = [
        (Stream::Records, KillPoint::MidWrite),
        (Stream::Records, KillPoint::BeforeRename),
        (Stream::Records, KillPoint::AfterRename),
        (Stream::Raw, KillPoint::MidWrite),
        (Stream::Raw, KillPoint::BeforeRename),
        (Stream::Raw, KillPoint::AfterRename),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("journal");
    let open = || Journal::open_with(&dir, Chain::Regtest, false, &options(segment)).unwrap();
    let mut journal = open();
    let mut model = Model {
        density,
        segment,
        files: BTreeMap::new(),
        staged: None,
    };
    let reads = Arc::new(AtomicU64::new(0));
    let spawn_reader = |journal: &Journal| {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let (reader, stop, reads) = (
                JournalReader::new(journal.clone()),
                stop.clone(),
                reads.clone(),
            );
            move || read_until(reader, density, stop, reads)
        });
        (stop, thread)
    };
    let mut reader = spawn_reader(&journal);
    let mut rollovers = BTreeSet::new();

    let mut next = 0;
    let mut round = 0;
    let mut kill = 0;
    while next < keep * windows {
        round += 1;
        let step = 2 + round % 5;
        for height in next..next + step {
            model.stage_through(&journal, height + ahead);
            model.finalize(&journal, height);
            if height % segment == 0 && height > 0 {
                // A rollover: both sides read back whole.
                let view = JournalReader::new(journal.clone());
                assert!(view.block_view(height - 1).unwrap().is_some());
                assert!(view.block_view(height).unwrap().is_some());
                rollovers.insert(height);
            }
        }
        next += step;
        model.check(&journal, keep, &format!("round {round} after appending"));

        if round % 3 == 0 {
            // A reorg 2 to 7 blocks deep, across a boundary when it falls
            // near one; ord's replay restages the heights above it.
            let agreed = next - 1 - (2 + round % 6);
            rollback_to(&journal, agreed).unwrap();
            journal.remove_raw_above(Some(agreed)).unwrap();
            model.files.retain(|(_, height), _| *height <= agreed);
            model.staged = Some(agreed);
            next = agreed + 1;
            model.check(
                &journal,
                keep,
                &format!("round {round} after a rollback to {agreed}"),
            );
        }

        if round % 2 == 0 {
            let (stream, point) = kills[kill % kills.len()];
            kill += 1;
            let (stop, thread) = reader;
            stop.store(true, Ordering::SeqCst);
            thread.join().unwrap();
            let file = match stream {
                Stream::Records => testing::records_file(next, density).unwrap(),
                Stream::Raw => raw_file(model.staged.unwrap() + 1, density),
            };
            testing::killed_write(&journal, stream, &file, point).unwrap();
            drop(journal);
            journal = open();
            if point == KillPoint::AfterRename {
                let len = testing::encoded_len(&file, stream).unwrap();
                model.files.insert((stream.dir_name(), file.height), len);
                match stream {
                    Stream::Records => next += 1,
                    Stream::Raw => model.staged = Some(file.height),
                }
            }
            model.check(
                &journal,
                keep,
                &format!("round {round} after a kill {point:?} in {stream:?}"),
            );
            reader = spawn_reader(&journal);
        }

        let tip = journal.final_tip().unwrap().unwrap();
        let below = (tip + 1).saturating_sub(keep);
        journal.prune(below, below).unwrap();
        let floor = journal.pruned_below().unwrap();
        model.files.retain(|(stream, height), _| match *stream {
            "rec" => *height >= floor,
            _ => *height >= below,
        });
        model.check(
            &journal,
            keep,
            &format!("round {round} after pruning below {below}"),
        );
    }
    let (stop, thread) = reader;
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap();
    assert!(kill >= kills.len(), "{kill} kills");
    assert!(
        rollovers.len() as u32 >= windows * keep / segment - 1,
        "{rollovers:?}"
    );
    assert!(reads.load(Ordering::SeqCst) > 0);
    println!(
        "indexed {next} heights in {round} rounds; {} reads, {kill} kills, rollovers {rollovers:?}",
        reads.load(Ordering::SeqCst)
    );
}
