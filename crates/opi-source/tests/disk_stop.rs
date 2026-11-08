//! The journal's free-space stop. One test per binary: the free space the
//! journal sees is process-wide.

use {
    bitcoin::{BlockHash, hashes::Hash},
    opi_source::journal::{
        DiskFull, Journal, JournalOptions, Stream,
        testing::{self, Density},
        testing_free,
    },
    ord::Chain,
};

const DENSITY: Density = Density {
    raw_events: 20,
    creations: 8,
    transfers: 4,
    content: 4096,
};

/// A write that would leave less than the minimum free fails with
/// [`DiskFull`] before it touches the journal, in both streams; a restart
/// once space is back resumes from the same tip.
#[test]
fn the_journal_stops_before_the_write_that_crosses_the_minimum() {
    let min_free = 20u64 << 30;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("journal");
    let options = JournalOptions {
        segment: 100,
        min_free_bytes: min_free,
        ..JournalOptions::default()
    };
    let open = || Journal::open_with(&dir, Chain::Regtest, false, &options).unwrap();

    testing_free::set(Some(min_free + (1 << 30)));
    let journal = open();
    testing::fill(&journal, 0..10, DENSITY).unwrap();

    let next = testing::records_file(10, DENSITY).unwrap();
    let size = testing::encoded_len(&next, Stream::Records).unwrap();
    // One byte short of the room the write needs stops it untouched.
    testing_free::set(Some(min_free + size - 1));
    let before = std::fs::read_dir(journal.dir().join("rec/000000000"))
        .unwrap()
        .count();
    let err = journal.append_records(&next).unwrap_err();
    assert!(err.is::<DiskFull>(), "{err:#}");
    assert!(
        format!("{err:#}").contains("OPI_MIN_FREE_DISK_GB"),
        "{err:#}"
    );
    assert_eq!(journal.final_tip().unwrap(), Some(9));
    assert_eq!(
        std::fs::read_dir(journal.dir().join("rec/000000000"))
            .unwrap()
            .count(),
        before
    );
    // Exactly the room it needs writes it.
    testing_free::set(Some(min_free + size));
    journal.append_records(&next).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(10));

    testing_free::set(Some(min_free));
    let err = journal
        .write_raw(
            11,
            BlockHash::all_zeros(),
            &testing::raw_events(11, DENSITY),
        )
        .unwrap_err();
    assert!(err.is::<DiskFull>(), "{err:#}");
    assert_eq!(journal.heights(Stream::Raw).unwrap().last(), Some(&9));

    drop(journal);
    testing_free::set(None);
    let journal = open();
    assert_eq!(journal.final_tip().unwrap(), Some(10));
    testing::fill(&journal, 11..20, DENSITY).unwrap();
    assert_eq!(journal.final_tip().unwrap(), Some(19));
}
