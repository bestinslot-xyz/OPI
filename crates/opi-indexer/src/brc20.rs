//! `opi-indexer brc20`: the BRC-20 light client and maintenance commands.
//!
//! These commands read only brc20's own environment
//! (`Brc20IndexerConfig::default()`). They never load the shared `Config`
//! or open the journal, so they run on a light-client host with no `OPI_*`
//! settings. `event-str` reads only Postgres and runs alongside a
//! running `opi-indexer run`. Every other command opens brc20-prog's database,
//! which one process holds at a time, so `run` must be stopped first.
//! `light` serves the metrics listener on METRICS_ADDR, with the OPI
//! network's best verified block as the chain tip, and the engine's public
//! JSON-RPC under the same settings as `run`. It stops after its current
//! block on SIGINT or SIGTERM. Every command that opens the engine runs on
//! the brc20 thread.

use {
    anyhow::{Result, anyhow, bail},
    clap::{Args as ClapArgs, Subcommand},
    opi_brc20::{
        config::{BitcoinRpcAuth, Brc20IndexerConfig},
        database::{Brc20Database, get_brc20_database, set_brc20_database},
        fail_stop::BRC20_THREAD_NAME,
        indexer::Brc20Indexer,
    },
    opi_core::metrics::Phase,
    std::{
        io::{self, BufRead, Write},
        sync::Arc,
        time::Duration,
    },
    tokio::{
        signal::unix::SignalKind,
        sync::{Mutex, watch},
    },
};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

/// Skips the y/N prompt of a destructive command.
#[derive(ClapArgs)]
pub struct Confirm {
    /// Skip the y/N confirmation prompt.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the BRC-20 light client until stopped. Requires OPERATION_MODE=light.
    /// Stop `run` first.
    Light,
    /// Delete all BRC-20 data and start fresh. Stop `run` first.
    Reset(Confirm),
    /// Roll the BRC-20 state back to a block height. Stop `run` first.
    Reorg {
        /// Height to roll back to.
        height: i32,
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Recompute the cumulative event hashes from a height to the latest indexed block.
    /// Stop `run` first.
    RecalculateHashes {
        /// First height to recompute.
        from: i32,
    },
    /// Rebuild the extra tables from the indexed data. May take a long time.
    /// Stop `run` first.
    ReindexExtras(Confirm),
    /// Print the block event string at a height. Runs alongside `run`.
    EventStr {
        /// Block height.
        height: i32,
    },
    /// Print the block trace string at a height. Stop `run` first.
    TraceStr {
        /// Block height.
        height: i32,
    },
}

/// Reads a y/N answer from `input`; `yes` answers without reading. Any
/// read failure or answer other than `y`/`yes` declines.
pub fn confirm_with(
    prompt: &str,
    yes: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> bool {
    if yes {
        return true;
    }
    if write!(output, "{prompt} [y/N]: ")
        .and_then(|()| output.flush())
        .is_err()
    {
        return false;
    }
    let mut answer = String::new();
    input.read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn confirm(prompt: &str, yes: bool) -> bool {
    confirm_with(prompt, yes, &mut io::stdin().lock(), &mut io::stdout())
}

/// Runs a brc20 command to completion on its own runtime.
pub fn run(args: Args) -> Result<()> {
    opi_brc20::fail_stop::install_panic_hook();
    // Checked first so that a bad combination is an error, not a panic in
    // the config's default.
    BitcoinRpcAuth::from_env()?;
    let config = Brc20IndexerConfig::default();
    let light = matches!(args.command, Command::Light);
    if light && !config.light_client_mode {
        bail!(
            "`brc20 light` runs the light client; set OPERATION_MODE=light. Full mode indexes inside `opi-indexer run` with OPI_MODULES including brc20."
        );
    }
    // The light client serves the metrics listener, as `run` does, from
    // here until it returns; it reports running once its start-up is done.
    let metrics = if light {
        crate::start_metrics(crate::config::MetricsConfig::from_env()?)?
    } else {
        None
    };
    let result = execute(args.command, config);
    if let Some(metrics) = metrics {
        metrics.stop();
    }
    result
}

/// Runs `command` on a runtime of its own. Every command but `event-str`
/// opens the engine first; `light` also serves the engine's public JSON-RPC
/// as `run` does, from before its start-up until it has stopped.
fn execute(command: Command, config: Brc20IndexerConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // `event-str` reads Postgres only, so it leaves the engine's database to
    // a running `opi-indexer run`.
    if let Command::EventStr { height } = command {
        return runtime.block_on(async {
            set_database(&config);
            event_str(height).await
        });
    }
    let light = matches!(command, Command::Light);
    let engine = crate::open_brc20_prog(&config)?;
    let public_rpc = public_rpc(&command, &engine, &config)?;
    let (stop, stopped) = watch::channel(false);
    let signals = light.then(|| runtime.spawn(stop_on_signal(stop)));
    let result = on_brc20_thread(move || async move {
        set_database(&config);
        execute_with_engine(command, config, engine, stopped).await
    });
    if light {
        crate::shut_down_brc20(&runtime, public_rpc);
    }
    // A second signal forces the exit until here.
    if let Some(signals) = signals {
        signals.abort();
    }
    result
}

/// Requests the light client's stop on the first SIGINT or SIGTERM; from then
/// on readiness answers shutting down. A second signal exits with status 130
/// once the engine's current commit has finished (see `forced_exit`).
/// Without a SIGTERM listener, SIGINT alone stops it. `stop`
/// stays open for the whole run, so the light client's waits never see it
/// closed.
async fn stop_on_signal(stop: watch::Sender<bool>) {
    let mut sigterm = match tokio::signal::unix::signal(SignalKind::terminate()) {
        Ok(sigterm) => Some(sigterm),
        Err(err) => {
            tracing::error!(
                "listening for SIGTERM failed: {err}; only SIGINT stops the light client"
            );
            None
        }
    };
    let name = next_signal(sigterm.as_mut()).await;
    tracing::info!("{name} received, stopping after the current block");
    opi_core::metrics::global().set_phase(Phase::ShuttingDown);
    stop.send(true).ok();
    let name = next_signal(sigterm.as_mut()).await;
    tracing::warn!("{name} received again");
    forced_exit(FORCED_EXIT_WAIT, |code| std::process::exit(code)).await;
}

/// How long a forced stop waits for the engine's current commit.
const FORCED_EXIT_WAIT: Duration = Duration::from_secs(30);

/// Exits through `exit` with status 130 once the engine's current commit has
/// finished, or after `limit` for a hung disk. The guard stays held, so no
/// engine write starts before the exit.
async fn forced_exit(limit: Duration, exit: impl FnOnce(i32)) {
    let guard = opi_brc20::engine_writes::wait_until_free(limit).await;
    if guard.is_some() {
        tracing::warn!("forced stop after the engine commit finished");
    } else {
        tracing::error!("forced stop: engine commit still running after {limit:?}");
    }
    exit(130);
    drop(guard);
}

/// The name of the next SIGINT or SIGTERM, read from `sigterm` when there is
/// one. A failed SIGINT listener waits on SIGTERM alone.
async fn next_signal(sigterm: Option<&mut tokio::signal::unix::Signal>) -> &'static str {
    let sigterm = async {
        match sigterm {
            Some(sigterm) => {
                sigterm.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(sigterm);
    let sigint = tokio::select! {
        result = tokio::signal::ctrl_c() => result.is_ok(),
        _ = &mut sigterm => return "SIGTERM",
    };
    if !sigint {
        sigterm.await;
        return "SIGTERM";
    }
    "SIGINT"
}

/// Runs `job`'s future to completion on a current-thread runtime on a thread
/// named `BRC20_THREAD_NAME`, as `run`'s brc20 module does: a panic there
/// stops the process (see `fail_stop`), and the engine's Bitcoin lookups from
/// it are the indexer's own, which public JSON-RPC calls are not.
fn on_brc20_thread<F, Fut>(job: F) -> Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>>,
{
    std::thread::Builder::new()
        .name(BRC20_THREAD_NAME.into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(job())
        })?
        .join()
        .map_err(|_| anyhow!("the brc20 thread panicked"))?
}

/// The engine's public JSON-RPC for `command`: `light` starts it under
/// `run`'s rule, and the maintenance commands run none.
pub(crate) fn public_rpc(
    command: &Command,
    engine: &crate::Brc20Engine,
    config: &Brc20IndexerConfig,
) -> Result<Option<crate::PublicRpc>> {
    match command {
        Command::Light => crate::start_public_rpc(engine, config),
        _ => Ok(None),
    }
}

fn set_database(config: &Brc20IndexerConfig) {
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(config))));
}

async fn event_str(height: i32) -> Result<()> {
    let database = get_brc20_database();
    let mut database = database.lock().await;
    database.init().await.map_err(|e| anyhow!("{e}"))?;
    match database
        .get_block_events_str(height)
        .await
        .map_err(|e| anyhow!("{e}"))?
    {
        Some(text) => println!("Block Event String at height {height}:\n{text}"),
        None => println!("No events found at height {height}"),
    }
    Ok(())
}

/// Runs a command that needs `engine`, which holds its database and its own
/// Bitcoin RPC source for the command's whole run. `light` stops once `stop`
/// holds true.
async fn execute_with_engine(
    command: Command,
    config: Brc20IndexerConfig,
    engine: crate::Brc20Engine,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut indexer = Brc20Indexer::new(config, engine.prog, engine.bitcoin_rpc_buffer, None);
    match command {
        Command::EventStr { .. } => unreachable!("event-str runs without the engine"),
        Command::Light => {
            indexer.stop_on(stop);
            indexer.run().await.map_err(|e| anyhow!("{e}"))?
        }
        Command::Reset(Confirm { yes }) => {
            if !confirm(
                "Are you sure you want to reset the indexer? This will delete all data and start fresh.",
                yes,
            ) {
                tracing::error!("Reset cancelled.");
                return Ok(());
            }
            indexer.reset().await.map_err(|e| anyhow!("{e}"))?;
            tracing::info!("Indexer reset successfully.");
        }
        Command::Reorg {
            height,
            confirm: Confirm { yes },
        } => {
            if !confirm(
                "Are you sure you want to reorg the indexer? This will reset the state to the specified height.",
                yes,
            ) {
                tracing::error!("Reorg cancelled.");
                return Ok(());
            }
            indexer.reorg(height).await.map_err(|e| anyhow!("{e}"))?;
            tracing::info!("Reorg to height {height} completed successfully.");
        }
        Command::RecalculateHashes { from } => {
            tracing::info!(
                "Recalculating cumulative event hashes from height {from} to the latest indexed block..."
            );
            get_brc20_database()
                .lock()
                .await
                .recalculate_cumulative_event_hashes(from)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            tracing::info!("Recalculation of cumulative event hashes completed successfully.");
        }
        Command::ReindexExtras(Confirm { yes }) => {
            if !confirm(
                "Are you sure you want to reindex extra data? This may take a long time.",
                yes,
            ) {
                tracing::error!("Reindexing cancelled.");
                return Ok(());
            }
            get_brc20_database()
                .lock()
                .await
                .initial_index_of_extra_tables()
                .await
                .map_err(|e| anyhow!("{e}"))?;
            tracing::info!("Reindexing of extra data completed successfully.");
        }
        Command::TraceStr { height } => {
            match indexer
                .get_block_trace_string(height)
                .await
                .map_err(|e| anyhow!("{e}"))?
            {
                Some(text) => println!("Block Trace String at height {height}:\n{text}"),
                None => println!("No traces found at height {height}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forced stop waits for the engine write in progress and exits once
    /// it has finished; a write that outlasts the limit is not waited for.
    #[tokio::test]
    async fn a_forced_stop_waits_for_the_engine_commit() {
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let commit = tokio::spawn(opi_brc20::engine_writes::guarded(async move {
            started.send(()).unwrap();
            finished.await.ok();
        }));
        wait_started.await.unwrap();

        let exited = Arc::new(std::sync::Mutex::new(None));
        let record = exited.clone();
        let exit = tokio::spawn(forced_exit(Duration::from_secs(30), move |code| {
            *record.lock().unwrap() = Some(code);
        }));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(*exited.lock().unwrap(), None, "exited during the commit");
        finish.send(()).unwrap();
        exit.await.unwrap();
        assert_eq!(*exited.lock().unwrap(), Some(130));
        commit.await.unwrap();

        // A hung commit: the exit goes ahead after the limit.
        let (_hold, hung) = tokio::sync::oneshot::channel::<()>();
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let hung_commit = tokio::spawn(opi_brc20::engine_writes::guarded(async move {
            started.send(()).unwrap();
            hung.await.ok();
        }));
        wait_started.await.unwrap();
        let record = exited.clone();
        *record.lock().unwrap() = None;
        forced_exit(Duration::from_millis(100), move |code| {
            *record.lock().unwrap() = Some(code);
        })
        .await;
        assert_eq!(*exited.lock().unwrap(), Some(130));
        hung_commit.abort();
    }

    /// Engine commands run on the brc20 thread, so their Bitcoin lookups are
    /// the indexer's own and fail-stop covers their panics; the job's
    /// result comes back.
    #[test]
    fn engine_commands_run_on_the_brc20_thread() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        on_brc20_thread(move || async move {
            *record.lock().unwrap() = std::thread::current().name().map(str::to_string);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.lock().unwrap().as_deref(), Some(BRC20_THREAD_NAME));
        assert_eq!(
            on_brc20_thread(|| async { Err(anyhow!("failed")) })
                .unwrap_err()
                .to_string(),
            "failed"
        );
    }
}
