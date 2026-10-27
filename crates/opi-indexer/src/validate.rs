//! `opi-indexer validate`: checks brc20's cumulative event and trace hashes
//! against the ones the OPI network publishes.
//!
//! Each block's hash folds into the next, so a match at height N vouches for
//! every height up to N. The check reads brc20's Postgres and the OPI API; it
//! never opens the journal and runs alongside `opi-indexer run`.

use {
    anyhow::{Result, anyhow, bail},
    clap::Args as ClapArgs,
    opi_brc20::{client::EventProviderClient, config::Brc20IndexerConfig, database::Brc20Database},
    std::fmt,
};

#[derive(ClapArgs)]
pub struct Args {
    /// First height to check. Defaults to brc20's first BRC-20 height for the
    /// network.
    #[arg(long)]
    pub from: Option<i32>,
    /// Last height to check, inclusive. Defaults to the lower of brc20's latest
    /// height and the OPI network's best verified block.
    #[arg(long)]
    pub to: Option<i32>,
}

/// brc20's cumulative hashes at one height. `trace` is empty when the height
/// has no traces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ours {
    pub event: String,
    pub trace: String,
}

/// The OPI network's cumulative hashes at one height. `trace` is `None` when
/// the network has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theirs {
    pub event: String,
    pub trace: Option<String>,
}

impl Ours {
    /// The event hashes are equal and, when the network has a trace hash, the
    /// trace hashes are equal.
    fn matches(&self, theirs: &Theirs) -> bool {
        self.event == theirs.event
            && theirs
                .trace
                .as_ref()
                .is_none_or(|trace| *trace == self.trace)
    }
}

/// The first height where the two sides differ.
#[derive(Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub height: i32,
    pub ours: Ours,
    pub theirs: Theirs,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "MISMATCH at height {}", self.height)?;
        writeln!(f, "event hash  ours:   {}", self.ours.event)?;
        writeln!(f, "event hash  theirs: {}", self.theirs.event)?;
        writeln!(f, "trace hash  ours:   {}", show(Some(&self.ours.trace)))?;
        write!(
            f,
            "trace hash  theirs: {}",
            show(self.theirs.trace.as_ref())
        )
    }
}

fn show(hash: Option<&String>) -> &str {
    match hash.map(String::as_str) {
        None => "(none)",
        Some("") => "(empty)",
        Some(hash) => hash,
    }
}

/// Finds the first height in `from..=to` whose hashes differ, or `None` when
/// the hashes match at `to`. A match at `to` vouches for every earlier height;
/// otherwise a bisection over the cumulative hashes finds the first
/// difference. A lookup error ends the search.
pub fn first_mismatch<O, T>(
    from: i32,
    to: i32,
    mut ours: O,
    mut theirs: T,
) -> Result<Option<Mismatch>>
where
    O: FnMut(i32) -> Result<Ours>,
    T: FnMut(i32) -> Result<Theirs>,
{
    if from > to {
        bail!("empty range {from}..{to}");
    }
    let mut agree = |height: i32| -> Result<bool> { Ok(ours(height)?.matches(&theirs(height)?)) };
    if agree(to)? {
        return Ok(None);
    }
    let (mut low, mut high) = (from, to);
    while low < high {
        let mid = low + (high - low) / 2;
        if agree(mid)? {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    Ok(Some(Mismatch {
        height: low,
        ours: ours(low)?,
        theirs: theirs(low)?,
    }))
}

/// Runs the check. `Ok(true)` means the hashes match over the range.
pub fn run(args: Args) -> Result<bool> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let config = Brc20IndexerConfig::default();
    let database = {
        let _guard = runtime.enter();
        Brc20Database::new(&config)
    };
    let client = EventProviderClient::new(&config).map_err(|err| anyhow!("{err}"))?;

    let from = args.from.unwrap_or(config.first_brc20_height);
    let to = match args.to {
        Some(to) => to,
        None => {
            let ours = runtime
                .block_on(database.get_current_block_height())
                .map_err(|err| anyhow!("reading brc20's latest height: {err}"))?;
            let theirs = runtime
                .block_on(client.get_best_verified_block_with_retries())
                .map_err(|err| anyhow!("reading the network's best verified block: {err}"))?;
            ours.min(theirs)
        }
    };
    if to < from {
        bail!("nothing to check: --to {to} is below --from {from}");
    }

    let ours = |height: i32| -> Result<Ours> {
        let event = runtime
            .block_on(database.get_cumulative_events_hash(height))
            .map_err(|err| anyhow!("reading brc20's hashes at height {height}: {err}"))?
            .ok_or_else(|| anyhow!("brc20 has no hashes at height {height}"))?;
        let trace = runtime
            .block_on(database.get_cumulative_traces_hash(height))
            .map_err(|err| anyhow!("reading brc20's hashes at height {height}: {err}"))?
            .unwrap_or_default();
        Ok(Ours { event, trace })
    };
    let theirs = |height: i32| -> Result<Theirs> {
        let block = runtime
            .block_on(client.get_block_info_with_retries(height))
            .map_err(|err| anyhow!("the network has no hashes at height {height}: {err}"))?;
        Ok(Theirs {
            event: block.best_cumulative_hash,
            trace: block.best_cumulative_trace_hash,
        })
    };

    match first_mismatch(from, to, ours, theirs)? {
        None => {
            println!("OK {from}..{to}");
            Ok(true)
        }
        Some(mismatch) => {
            println!("{mismatch}");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::cell::Cell};

    fn event(height: i32) -> String {
        format!("event-{height}")
    }

    fn trace(height: i32) -> String {
        format!("trace-{height}")
    }

    /// Hashes that agree below `diverge_at`; from there every height differs,
    /// the way cumulative hashes do. `trace_only` keeps the event hashes equal.
    fn sides(
        diverge_at: i32,
        trace_only: bool,
        network_trace: bool,
    ) -> (
        impl FnMut(i32) -> Result<Ours>,
        impl FnMut(i32) -> Result<Theirs>,
    ) {
        let ours = |height: i32| {
            Ok(Ours {
                event: event(height),
                trace: trace(height),
            })
        };
        let theirs = move |height: i32| {
            let diverged = height >= diverge_at;
            Ok(Theirs {
                event: if diverged && !trace_only {
                    format!("other-{height}")
                } else {
                    event(height)
                },
                trace: network_trace.then(|| {
                    if diverged && trace_only {
                        format!("other-{height}")
                    } else {
                        trace(height)
                    }
                }),
            })
        };
        (ours, theirs)
    }

    #[test]
    fn all_match() {
        let (ours, theirs) = sides(i32::MAX, false, true);
        assert_eq!(first_mismatch(10, 500, ours, theirs).unwrap(), None);
    }

    #[test]
    fn all_match_checks_only_the_last_height() {
        let calls = Cell::new(0);
        let (mut ours, theirs) = sides(i32::MAX, false, true);
        let counting = |height| {
            calls.set(calls.get() + 1);
            ours(height)
        };
        assert_eq!(first_mismatch(10, 500, counting, theirs).unwrap(), None);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn first_event_mismatch_is_found() {
        for diverge_at in [10, 11, 137, 499, 500] {
            let (ours, theirs) = sides(diverge_at, false, true);
            let found = first_mismatch(10, 500, ours, theirs).unwrap().unwrap();
            assert_eq!(found.height, diverge_at);
            assert_eq!(found.ours.event, event(diverge_at));
            assert_eq!(found.theirs.event, format!("other-{diverge_at}"));
        }
    }

    #[test]
    fn trace_only_mismatch_is_found() {
        let (ours, theirs) = sides(321, true, true);
        let found = first_mismatch(10, 500, ours, theirs).unwrap().unwrap();
        assert_eq!(found.height, 321);
        assert_eq!(found.ours.event, found.theirs.event);
        assert_eq!(found.ours.trace, trace(321));
        assert_eq!(found.theirs.trace, Some("other-321".into()));
    }

    #[test]
    fn null_network_trace_is_ignored() {
        let (ours, theirs) = sides(i32::MAX, true, false);
        assert_eq!(first_mismatch(10, 500, ours, theirs).unwrap(), None);
        // An event mismatch is still found.
        let (ours, theirs) = sides(77, false, false);
        let found = first_mismatch(10, 500, ours, theirs).unwrap().unwrap();
        assert_eq!(found.height, 77);
        assert_eq!(found.theirs.trace, None);
    }

    #[test]
    fn network_trace_against_an_empty_local_trace_mismatches() {
        let ours = |_| {
            Ok(Ours {
                event: "e".into(),
                trace: String::new(),
            })
        };
        let theirs = |_| {
            Ok(Theirs {
                event: "e".into(),
                trace: Some("t".into()),
            })
        };
        let found = first_mismatch(5, 5, ours, theirs).unwrap().unwrap();
        assert_eq!(found.height, 5);
    }

    #[test]
    fn single_height_range() {
        let (ours, theirs) = sides(i32::MAX, false, true);
        assert_eq!(first_mismatch(42, 42, ours, theirs).unwrap(), None);
        let (ours, theirs) = sides(42, false, true);
        let found = first_mismatch(42, 42, ours, theirs).unwrap().unwrap();
        assert_eq!(found.height, 42);
    }

    #[test]
    fn missing_height_is_an_error() {
        let (_, theirs) = sides(i32::MAX, false, true);
        let missing = |height| Err(anyhow!("brc20 has no hashes at height {height}"));
        let err = first_mismatch(10, 20, missing, theirs).unwrap_err();
        assert!(err.to_string().contains("height 20"), "{err}");

        let (ours, _) = sides(i32::MAX, false, true);
        let missing = |height| Err(anyhow!("the network has no hashes at height {height}"));
        assert!(first_mismatch(10, 20, ours, missing).is_err());
    }

    #[test]
    fn empty_range_is_an_error() {
        let (ours, theirs) = sides(i32::MAX, false, true);
        assert!(first_mismatch(20, 10, ours, theirs).is_err());
    }
}
