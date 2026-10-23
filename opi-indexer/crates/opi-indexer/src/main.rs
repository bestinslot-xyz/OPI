mod config;

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
    opi_source::driver::{SourceHandle, spawn_source},
    std::{
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
        Command::Run => run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let config = Config::from_env()?;
    let network = config.network();

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
    let config = Brc20IndexerConfig::default();
    if config.network_type != network {
        bail!(
            "NETWORK_TYPE {:?} does not match OPI_CHAIN network {network:?}",
            config.network_type
        );
    }
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(&config))));

    let mut backoff = MIN_BACKOFF;
    loop {
        let source = JournalBrc20Source::new(Arc::new(handle.reader()), network);
        let mut indexer = Brc20Indexer::new(Brc20IndexerConfig::default(), Box::new(source));
        let started = Instant::now();
        let result = indexer.run().await.map_err(|err| err.to_string());
        match result {
            Ok(()) => {
                tracing::info!("brc20 indexer finished");
                std::future::pending::<()>().await;
            }
            Err(err) => {
                if started.elapsed() > MAX_BACKOFF {
                    backoff = MIN_BACKOFF;
                }
                tracing::error!("brc20 indexer failed, restarting in {backoff:?}: {err}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}
