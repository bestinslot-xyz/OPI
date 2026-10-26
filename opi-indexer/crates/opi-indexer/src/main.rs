mod config;
mod validate;

use {
    anyhow::{Result, bail},
    brc20_index::{
        client::JournalBrc20Source,
        config::Brc20IndexerConfig,
        database::{Brc20Database, get_brc20_database, set_brc20_database},
        indexer::Brc20Indexer,
    },
    clap::{Parser, Subcommand},
    config::Config,
    futures::FutureExt,
    opi_core::BlockSource,
    opi_source::driver::{SourceHandle, spawn_source},
    std::{
        any::Any,
        future::Future,
        panic::AssertUnwindSafe,
        process::ExitCode,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    },
    tokio::{signal::unix::SignalKind, sync::Mutex},
    tracing_subscriber::EnvFilter,
};

const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const SOURCE_POLL: Duration = Duration::from_secs(1);
const TIP_POLL: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(
    version,
    about = "Runs the journal source and the OPI modules in one process"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Index until SIGINT or SIGTERM (default).
    Run,
    /// Check brc20's cumulative event and trace hashes against the OPI
    /// network's. Runs while `run` is running. Exits 0 when the hashes match,
    /// 1 at the first mismatch, 2 on an error such as a height missing on
    /// either side.
    Validate(validate::Args),
}

/// Why the supervisor stopped.
enum Exit {
    Signal,
    SourceEnded,
}

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().map(|()| true),
        Command::Validate(args) => {
            return match validate::run(args) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(err) => {
                    tracing::error!("{err:#}");
                    ExitCode::from(2)
                }
            };
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let config = Config::from_env()?;
    let network = config.network();
    if config.brc20_enabled {
        let brc20 = Brc20IndexerConfig::default();
        if brc20.network_type != network {
            bail!(
                "NETWORK_TYPE {:?} does not match OPI_CHAIN network {network:?}",
                brc20.network_type
            );
        }
    }

    // The source blocks on its own runtime, so it starts and stops outside
    // the runtime below.
    let handle = spawn_source(config.source.clone())?;

    let exit = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(supervise(&handle, config.brc20_enabled, network)));

    handle.shutdown();
    let joined = handle.join();
    match exit {
        Ok(Exit::Signal) => joined,
        Ok(Exit::SourceEnded) => {
            joined?;
            bail!("source stopped unexpectedly")
        }
        Err(err) => {
            if let Err(source_err) = joined {
                tracing::error!("source: {source_err:#}");
            }
            Err(err)
        }
    }
}

async fn supervise(
    handle: &SourceHandle,
    brc20_enabled: bool,
    network: bitcoin::Network,
) -> Result<Exit> {
    let brc20 = async {
        if brc20_enabled {
            run_brc20(handle, network).await
        } else {
            std::future::pending().await
        }
    };
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
    tokio::select! {
        result = brc20 => result.map(|()| Exit::Signal),
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, shutting down");
            Ok(Exit::Signal)
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
            Ok(Exit::Signal)
        }
        _ = source_finished(handle) => {
            tracing::error!("source driver exited");
            Ok(Exit::SourceEnded)
        }
    }
}

async fn source_finished(handle: &SourceHandle) {
    while !handle.is_finished() {
        tokio::time::sleep(SOURCE_POLL).await;
    }
}

/// Runs brc20 against the journal once the journal has a finalized block,
/// rebuilding the indexer and its database after every error with
/// exponential backoff. Returns only on a configuration error. When the
/// indexer ends on its own (height limit) it idles until the supervisor stops.
async fn run_brc20(handle: &SourceHandle, network: bitcoin::Network) -> Result<()> {
    wait_for_tip(&handle.reader(), TIP_POLL).await;
    let installed = &AtomicBool::new(false);
    restart_loop(
        || async {
            let config = Brc20IndexerConfig::default();
            install_fresh_database(installed, &config).await;
            let source = JournalBrc20Source::new(Arc::new(handle.reader()), network);
            let mut indexer = Brc20Indexer::new(config, Box::new(source));
            indexer.run().await.map_err(|err| err.to_string())
        },
        MIN_BACKOFF,
        MAX_BACKOFF,
    )
    .await
}

/// Polls `source` every `poll` until it reports a tip.
async fn wait_for_tip(source: &impl BlockSource, poll: Duration) {
    loop {
        match source.tip() {
            Ok(Some(_)) => return,
            Ok(None) => tracing::info!("waiting for the journal's first finalized block"),
            Err(err) => tracing::warn!("reading the journal tip: {err:#}"),
        }
        tokio::time::sleep(poll).await;
    }
}

/// Puts a freshly constructed database into brc20's global slot, so an
/// attempt starts from what Postgres holds, with empty caches and no pending
/// writes. `installed` records whether the slot is set; the slot is set once
/// and its contents are replaced on every later call.
async fn install_fresh_database(installed: &AtomicBool, config: &Brc20IndexerConfig) {
    let database = Brc20Database::new(config);
    if installed.swap(true, Ordering::SeqCst) {
        *get_brc20_database().lock().await = database;
    } else {
        set_brc20_database(Arc::new(Mutex::new(database)));
    }
}

/// Calls `attempt` until the supervisor drops this future. An error or a panic
/// is logged and followed by a backoff that doubles from `min` to `max`; a run
/// that lasted longer than `max` resets it. An `Ok` ends the loop's work and
/// idles.
async fn restart_loop<F, Fut>(mut attempt: F, min: Duration, max: Duration) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut backoff = min;
    loop {
        let started = Instant::now();
        let outcome = AssertUnwindSafe(async { attempt().await })
            .catch_unwind()
            .await;
        let err = match outcome {
            Ok(Ok(())) => {
                tracing::info!("brc20 indexer finished");
                std::future::pending::<()>().await;
                unreachable!()
            }
            Ok(Err(err)) => err,
            Err(payload) => format!("panic: {}", panic_message(payload.as_ref())),
        };
        if started.elapsed() > max {
            backoff = min;
        }
        tracing::error!("brc20 indexer failed, restarting in {backoff:?}: {err}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max);
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string payload".into()
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::sync::atomic::AtomicUsize};

    #[tokio::test]
    async fn restart_loop_restarts_after_panic_and_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let loop_future = restart_loop(
            move || {
                let call = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    match call {
                        0 => panic!("boom"),
                        1 => Err("failed".to_string()),
                        _ => Ok(()),
                    }
                }
            },
            Duration::from_millis(1),
            Duration::from_millis(4),
        );
        // The loop idles after the third call returns Ok.
        let timed_out = tokio::time::timeout(Duration::from_millis(500), loop_future)
            .await
            .is_err();
        assert!(timed_out);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// Runs the restart loop with attempts that install the database the way
    /// `run_brc20` does, then leave it dirty the way a mid-block failure does.
    /// The database's pool connects lazily, so no Postgres is needed.
    #[tokio::test]
    async fn each_attempt_starts_from_a_fresh_database() {
        let installed = &AtomicBool::new(false);
        let calls = &AtomicUsize::new(0);
        let seen = &std::sync::Mutex::new(Vec::new());
        let loop_future = restart_loop(
            || async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                install_fresh_database(installed, &Brc20IndexerConfig::default()).await;
                {
                    let database = get_brc20_database();
                    let mut database = database.lock().await;
                    seen.lock().unwrap().push((
                        database.block_event_strings.len(),
                        database.event_inserts.len(),
                        database.current_event_id,
                    ));
                    database
                        .block_event_strings
                        .insert(840_000, "partial".into());
                    database.current_event_id = 42;
                }
                match call {
                    0 => panic!("boom"),
                    1 => Err("failed".to_string()),
                    _ => Ok(()),
                }
            },
            Duration::from_millis(1),
            Duration::from_millis(4),
        );
        let timed_out = tokio::time::timeout(Duration::from_millis(500), loop_future)
            .await
            .is_err();
        assert!(timed_out);
        assert_eq!(*seen.lock().unwrap(), [(0, 0, 0); 3]);
        let database = get_brc20_database();
        let database = database.lock().await;
        assert_eq!(database.block_event_strings.len(), 1);
    }

    struct TipAfter {
        polls: AtomicUsize,
        ready_at: usize,
    }

    impl BlockSource for TipAfter {
        fn tip(&self) -> Result<Option<u32>> {
            let poll = self.polls.fetch_add(1, Ordering::SeqCst);
            match poll {
                0 => Err(anyhow::anyhow!("journal busy")),
                _ if poll < self.ready_at => Ok(None),
                _ => Ok(Some(840_000)),
            }
        }

        fn block_info(&self, _: u32) -> Result<Option<opi_core::BlockInfo>> {
            unreachable!()
        }

        fn inscription_records(&self, _: u32) -> Result<Vec<opi_core::InscriptionRecord>> {
            unreachable!()
        }

        fn inscription_info(&self, _: &str) -> Result<Option<opi_core::InscriptionInfo>> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn wait_for_tip_returns_once_the_journal_has_a_tip() {
        let source = TipAfter {
            polls: AtomicUsize::new(0),
            ready_at: 3,
        };
        tokio::time::timeout(
            Duration::from_millis(500),
            wait_for_tip(&source, Duration::from_millis(1)),
        )
        .await
        .unwrap();
        assert_eq!(source.polls.load(Ordering::SeqCst), 4);
    }

    /// This binary links brc20-index, which turns on serde_json's
    /// `arbitrary_precision`; the first assertion pins that the test runs
    /// under that feature set.
    #[test]
    fn is_json_matches_the_fork_under_arbitrary_precision() {
        assert!(serde_json::from_str::<serde_json::Value>("1e400").is_ok());
        let huge_integer = format!("1{}", "0".repeat(400));
        let is_json = opi_source::journal::is_json_body;
        assert!(!is_json(br#"{"a":1e400}"#));
        assert!(!is_json(br#"{"a":-1e400}"#));
        assert!(!is_json(huge_integer.as_bytes()));
        assert!(is_json(br#"{"a":1e-400}"#));
        assert!(is_json(
            br#"{"p":"brc-20","op":"transfer","tick":"ordi","amt":"1.5"}"#
        ));
    }
}
