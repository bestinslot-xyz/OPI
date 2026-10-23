mod config;
mod filters;
mod validate;

use {
    anyhow::{Result, bail},
    brc20_index::{
        client::JournalBrc20Source,
        config::Brc20IndexerConfig,
        database::{Brc20Database, set_brc20_database},
        indexer::Brc20Indexer,
    },
    clap::{Parser, Subcommand},
    config::Config,
    futures::FutureExt,
    opi_source::driver::{SourceHandle, spawn_source},
    std::{
        any::Any,
        future::Future,
        panic::AssertUnwindSafe,
        process::ExitCode,
        sync::Arc,
        time::{Duration, Instant},
    },
    tokio::{signal::unix::SignalKind, sync::Mutex},
    tracing_subscriber::EnvFilter,
};

const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const SOURCE_POLL: Duration = Duration::from_secs(1);

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
    /// Compare the journal with a running db_reader, block by block. Needs
    /// the journal, so opi-indexer must be stopped.
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
            Config::from_env().and_then(|config| validate::run(args, &config))
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

/// Runs brc20 against the journal, rebuilding the indexer after every error
/// with exponential backoff. Returns only on a configuration error. When the
/// indexer ends on its own (height limit) it idles until the supervisor stops.
async fn run_brc20(handle: &SourceHandle, network: bitcoin::Network) -> Result<()> {
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(
        &Brc20IndexerConfig::default(),
    ))));
    restart_loop(
        || async {
            let source = JournalBrc20Source::new(Arc::new(handle.reader()), network);
            let mut indexer = Brc20Indexer::new(Brc20IndexerConfig::default(), Box::new(source));
            indexer.run().await.map_err(|err| err.to_string())
        },
        MIN_BACKOFF,
        MAX_BACKOFF,
    )
    .await
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
    use {
        super::*,
        std::sync::atomic::{AtomicUsize, Ordering},
    };

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
}
