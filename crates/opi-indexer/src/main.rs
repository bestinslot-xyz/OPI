mod brc20;
mod config;
mod prune;
mod validate;

use {
    anyhow::{Context, Result, anyhow, bail},
    brc20_prog::{BitcoinRpc, Brc20Prog, Brc20ProgConfig},
    clap::{Parser, Subcommand},
    config::{Config, MetricsConfig, ModuleConfigs},
    futures::future::BoxFuture,
    jsonrpsee::server::ServerHandle,
    opi_api::metrics::MetricsThread,
    opi_bitmap::{BitmapConfig, BitmapModule},
    opi_brc20::{
        config::Brc20IndexerConfig,
        fail_stop::RPC_THREAD_NAME,
        indexer::{BitcoinRpcBufferSlot, CachedBitcoinRpc},
        module::Brc20Module,
        service_runtime::ServiceRuntime,
    },
    opi_core::{BlockSource, Module, metrics::Phase},
    opi_inscriptions::{InscriptionsConfig, InscriptionsModule},
    opi_runes::{RunesConfig, RunesModule},
    opi_runtime::{ModuleFactory, Runtime, RuntimeOptions},
    opi_sns::{SnsConfig, SnsModule},
    opi_source::driver::{SourceHandle, spawn_source},
    std::{process::ExitCode, sync::Arc, time::Duration},
    tokio::{signal::unix::SignalKind, sync::oneshot},
    tracing_subscriber::EnvFilter,
};

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
    /// Check brc20's (BRC-20 only) cumulative event and trace hashes against the OPI
    /// network's. Runs while `run` is running. Exits 0 when the hashes match,
    /// 1 at the first mismatch, 2 on an error such as a height missing on
    /// either side.
    Validate(validate::Args),
    /// BRC-20 light client and maintenance commands. Reads only brc20's
    /// environment, so it runs on light-client hosts. `event-str` runs
    /// alongside `run`; every other command needs `run` stopped, since it
    /// opens brc20-prog's database.
    Brc20(brc20::Args),
    /// Serve the OPI HTTP API for the modules OPI_MODULES enables, on
    /// API_HOST:API_PORT, until SIGINT or SIGTERM. Runs alongside `run`.
    /// Reads the env file OPI_ENV_FILE names, or `.env`.
    Api,
    /// Rewrite the journal file without the space pruning freed. Needs `run`
    /// stopped. Reads OPI_JOURNAL_DIR. With OPI_PRUNE set, it first prunes
    /// the journal to that window, from the heights in the enabled modules'
    /// databases, and reads `run`'s whole configuration.
    CompactJournal,
}

/// Why the modules stopped.
enum Exit {
    Signal,
    SourceEnded,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::Api)) {
        opi_api::config::load_env();
    } else {
        dotenvy::dotenv().ok();
    }
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().map(|()| true),
        Command::Brc20(args) => brc20::run(args).map(|()| true),
        Command::Api => api().map(|()| true),
        Command::CompactJournal => compact_journal().map(|()| true),
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
    opi_brc20::fail_stop::install_panic_hook();
    let config = Config::from_env()?;
    let module_configs = ModuleConfigs::load(&config)?;
    // Serves from here on, answering "starting" until the modules run, and
    // stops when `run` returns.
    let metrics = start_metrics(MetricsConfig::from_env()?)?;
    let result = match config.prune {
        Some(keep) => prune_then_index(&config, module_configs, keep),
        None => index(&config, module_configs),
    };
    if let Some(metrics) = metrics {
        metrics.stop();
    }
    result
}

/// `run` with OPI_PRUNE set: prunes the journal to the window before
/// anything takes it for writing, then indexes. The prune reads the module
/// heights from their databases, and an unreadable one fails the start. It
/// stops on SIGINT or SIGTERM, during the height reads or between two
/// steps, which then ends `run` with success.
fn prune_then_index(config: &Config, module_configs: ModuleConfigs, keep: u32) -> Result<()> {
    prune::watch_signals()?;
    let modules = prune::module_dbs(&module_configs)?;
    let path = config.source.journal_path();
    let lowest = match prune::start_heights(&path, &modules, &prune::signalled)? {
        prune::StartHeights::Lowest(lowest) => lowest,
        prune::StartHeights::Stopped => {
            tracing::info!("signal received while reading the module heights, shutting down");
            return Ok(());
        }
    };
    // `None` is a signal during the catch-up, which ends `run` with success.
    prune::catch_up_then(
        &path,
        &config.source.journal,
        config.source.chain,
        config.source.runes,
        lowest,
        keep,
        &prune::signalled,
        || index(config, module_configs),
    )
    .map(|_| ())
}

/// Starts the brc20 engine, the source and the modules, and runs until a
/// signal or a failure.
fn index(config: &Config, module_configs: ModuleConfigs) -> Result<()> {
    // The engine and its Bitcoin RPC source open once, before anything
    // starts; module restarts reuse them.
    let brc20_engine = module_configs
        .brc20
        .as_ref()
        .map(open_brc20_prog)
        .transpose()?;

    // Serves for the whole process, so brc20 module restarts keep it.
    let public_rpc = match (&brc20_engine, &module_configs.brc20) {
        (Some(engine), Some(brc20)) => start_public_rpc(engine, brc20)?,
        _ => None,
    };

    // The source blocks on its own runtime, so it starts and stops outside
    // the runtime below.
    let handle = match spawn_source(config.source.clone()) {
        Ok(handle) => handle,
        Err(err) => {
            if let Some(public_rpc) = public_rpc {
                public_rpc.stop();
            }
            return Err(err);
        }
    };

    let brc20_enabled = brc20_engine.is_some();
    let exit = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            opi_core::metrics::global().set_phase(Phase::Running);
            let exit = runtime.block_on(supervise(
                &handle,
                module_configs,
                brc20_engine,
                config.prune,
            ));
            // Every exit, an error one included, reports shutting down while
            // brc20 goes idle and the RPC server and source stop.
            if brc20_enabled {
                shut_down_brc20(&runtime, public_rpc);
            } else {
                opi_core::metrics::global().set_phase(Phase::ShuttingDown);
            }
            exit
        }
        Err(err) => {
            if let Some(public_rpc) = public_rpc {
                public_rpc.stop();
            }
            Err(err.into())
        }
    };

    // The modules and the public RPC server have stopped; the source stops
    // last.
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

/// Starts the metrics listener of `run` and `brc20 light` on METRICS_ADDR,
/// on a thread of its own, before the process starts its work: it answers
/// liveness during start-up, and a taken address fails the start. Readiness
/// follows READY_MAX_LAG_BLOCKS and READY_MAX_TIP_AGE_SECS. `None` when
/// METRICS_ADDR is unset.
pub(crate) fn start_metrics(config: MetricsConfig) -> Result<Option<MetricsThread>> {
    let Some(addr) = &config.addr else {
        return Ok(None);
    };
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("listening for metrics on {addr}"))?;
    let readiness = opi_api::metrics::indexer_readiness(config.ready_limits());
    MetricsThread::start(listener, readiness).map(Some)
}

/// Serves the HTTP API until SIGINT or SIGTERM. It runs no engine, so it
/// keeps the default panic hook.
fn api() -> Result<()> {
    let config = opi_api::config::load_config(|key| std::env::var(key).ok())?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
            let shutdown = async move {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received, shutting down"),
                    _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
                }
            };
            opi_api::serve(config, shutdown).await
        })
}

/// Compacts the journal under OPI_JOURNAL_DIR. With OPI_PRUNE set it prunes
/// the journal to the window first, from the heights in the databases of
/// the modules OPI_MODULES enables, reading `run`'s whole configuration.
fn compact_journal() -> Result<()> {
    if std::env::var("OPI_PRUNE").is_ok_and(|keep| !keep.is_empty()) {
        let config = Config::from_env()?;
        let keep = config.prune.context("OPI_PRUNE is set")?;
        let modules = prune::module_dbs(&ModuleConfigs::load(&config)?)?;
        prune::watch_signals()?;
        let offline = prune::OfflinePrune {
            chain: config.source.chain,
            runes: config.source.runes,
            keep,
            modules: &modules,
        };
        return prune::compact_command(
            &config.source.journal_path(),
            &config.source.journal,
            Some(offline),
            &prune::signalled,
        );
    }
    let dir = std::env::var("OPI_JOURNAL_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .context("OPI_JOURNAL_DIR is not set")?;
    let options = config::journal_options(&|key| std::env::var(key).ok())?;
    let path = std::path::Path::new(&dir).join(opi_source::driver::JOURNAL_FILE);
    prune::compact_command(&path, &options, None, &|| false)
}

/// Runs the enabled modules over the journal until a signal arrives or the
/// source driver exits, then stops them. With `prune` set, the modules prune
/// their history and a pruner task prunes the journal, each keeping the last
/// `prune` blocks.
async fn supervise(
    handle: &SourceHandle,
    configs: ModuleConfigs,
    brc20_engine: Option<Brc20Engine>,
    prune: Option<u32>,
) -> Result<Exit> {
    let reader = handle.reader();
    let journal: Arc<dyn BlockSource> = Arc::new(reader.clone());
    let mut runtime = Runtime::with_options(
        journal.clone(),
        RuntimeOptions {
            prune,
            ..RuntimeOptions::default()
        },
    );
    let mut names = Vec::new();
    if let Some(engine) = brc20_engine {
        runtime.add("brc20", brc20_factory(engine, journal.clone()));
        names.push("brc20");
    }
    if let Some(config) = configs.bitmap {
        runtime.add("bitmap", bitmap_factory(config, journal.clone()));
        names.push("bitmap");
    }
    if let Some(config) = configs.sns {
        runtime.add("sns", sns_factory(config, journal.clone()));
        names.push("sns");
    }
    if let Some(config) = configs.runes {
        runtime.add("runes", runes_factory(config, journal));
        names.push("runes");
    }
    if let Some(config) = configs.inscriptions {
        runtime.add("inscriptions", inscriptions_factory(config));
        names.push("inscriptions");
    }
    let pruner =
        prune.map(|keep| tokio::spawn(prune::prune_journal(reader.journal().clone(), names, keep)));

    let (stop, stopped) = oneshot::channel::<()>();
    let modules = runtime.run(async {
        stopped.await.ok();
    });
    tokio::pin!(modules);

    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
    let exit = tokio::select! {
        result = &mut modules => {
            result?;
            bail!("the modules stopped without a shutdown")
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, shutting down");
            Exit::Signal
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
            Exit::Signal
        }
        // A signal during start-up, after the journal catch-up installed its
        // handlers and before the two above listened.
        _ = prune::wait_signalled() => {
            tracing::info!("signal received during start-up, shutting down");
            Exit::Signal
        }
        _ = source_finished(handle) => {
            tracing::error!("source driver exited");
            Exit::SourceEnded
        }
    };
    // Readiness answers 503 from here on.
    opi_core::metrics::global().set_phase(Phase::ShuttingDown);
    if let Some(pruner) = pruner {
        pruner.abort();
        pruner.await.ok();
    }
    stop.send(()).ok();
    modules.await?;
    Ok(exit)
}

/// The BRC2.0 engine and the buffer slot of the Bitcoin RPC source it reads
/// Bitcoin data from. Each brc20 indexer installs its database's buffer there.
#[derive(Clone)]
pub(crate) struct Brc20Engine {
    pub(crate) prog: Brc20Prog,
    pub(crate) bitcoin_rpc_buffer: BitcoinRpcBufferSlot,
}

/// Opens the BRC2.0 engine from brc20's environment and brc20-prog's own,
/// with a `CachedBitcoinRpc` for `config` as its source of Bitcoin data. The
/// engine holds the source for the rest of the process.
pub(crate) fn open_brc20_prog(config: &Brc20IndexerConfig) -> Result<Brc20Engine> {
    let engine_config = config.brc20_prog_config()?;
    let source = CachedBitcoinRpc::new(config)?;
    let bitcoin_rpc_buffer = source.buffer_slot().clone();
    let prog = open_engine(engine_config, Arc::new(source))?;
    Ok(Brc20Engine {
        prog,
        bitcoin_rpc_buffer,
    })
}

/// Opens the engine at `engine_config.db_path` on `bitcoin_rpc`. RocksDB
/// admits one process per database, so a lock failure means another
/// `opi-indexer` holds it.
fn open_engine(
    engine_config: Brc20ProgConfig,
    bitcoin_rpc: Arc<dyn BitcoinRpc>,
) -> Result<Brc20Prog> {
    let db_path = engine_config.db_path.clone();
    Brc20Prog::open_with_bitcoin_rpc(engine_config, bitcoin_rpc).map_err(|e| {
        let message = e.to_string();
        if is_lock_error(&message) {
            anyhow!("brc20-prog's database at {db_path} is in use; stop `opi-indexer run` first")
        } else {
            anyhow!("failed to open the brc20-prog engine: {message}")
        }
    })
}

/// Whether a RocksDB open failure reports a held database lock: another
/// process holds the `LOCK` file, or this process already has the database
/// open.
fn is_lock_error(message: &str) -> bool {
    message.contains("lock hold by current process")
        || message.contains("While lock file")
            && ["Resource temporarily unavailable", "Permission denied"]
                .iter()
                .any(|reason| message.contains(reason))
}

/// How long shutdown waits for the brc20 thread to finish its last request.
/// It covers a stalled Bitcoin lookup's retries: 6 attempts bounded at 20 s
/// each take about 125 s.
const BRC20_IDLE_TIMEOUT: Duration = Duration::from_secs(150);

/// Waits up to `BRC20_IDLE_TIMEOUT` for the brc20 thread to go idle.
async fn wait_for_brc20_idle() {
    tracing::info!("waiting for brc20 to finish its last request");
    match tokio::time::timeout(BRC20_IDLE_TIMEOUT, opi_brc20::module::wait_until_idle()).await {
        Ok(()) => tracing::info!("brc20 is idle"),
        Err(_) => tracing::warn!("brc20 did not go idle within {BRC20_IDLE_TIMEOUT:?}"),
    }
}

/// Starts the engine's public JSON-RPC on `BRC20_PROG_RPC_SERVER_URL` for
/// `run` and `brc20 light`, when `BRC20_PROG_RPC_SERVER_ENABLED` turns it on.
/// Call it outside an async runtime, since it waits for the server's own.
pub(crate) fn start_public_rpc(
    engine: &Brc20Engine,
    config: &Brc20IndexerConfig,
) -> Result<Option<PublicRpc>> {
    if !config.brc20_prog_rpc_server_enabled {
        return Ok(None);
    }
    PublicRpc::start(&engine.prog).map(Some)
}

/// Ends the brc20 side of `run` and `brc20 light`: readiness answers
/// shutting down, brc20 gets up to `BRC20_IDLE_TIMEOUT` on `runtime` to
/// finish its last request, such as an engine commit, which the public RPC
/// must outlive, and then the public RPC stops.
pub(crate) fn shut_down_brc20(runtime: &tokio::runtime::Runtime, public_rpc: Option<PublicRpc>) {
    opi_core::metrics::global().set_phase(Phase::ShuttingDown);
    runtime.block_on(wait_for_brc20_idle());
    if let Some(public_rpc) = public_rpc {
        public_rpc.stop();
    }
}

/// How long `PublicRpc::stop` waits for in-flight public calls to finish.
const RPC_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The engine's public JSON-RPC server and the runtime it runs on. The
/// runtime has threads of its own, since the engine blocks the thread that
/// calls it.
pub(crate) struct PublicRpc {
    server: ServerHandle,
    runtime: ServiceRuntime,
}

impl PublicRpc {
    /// Starts the server on `brc20_prog_rpc_server_url`.
    fn start(brc20_prog: &Brc20Prog) -> Result<Self> {
        let runtime = ServiceRuntime::new(RPC_THREAD_NAME)?;
        let server = runtime
            .handle()
            .block_on(brc20_prog.start_public_rpc_server())
            .map_err(|e| anyhow!("failed to start the brc20-prog RPC server: {e}"))?;
        Ok(PublicRpc { server, runtime })
    }

    /// Stops the server, waits up to `RPC_STOP_TIMEOUT` for it to finish,
    /// then stops the runtime.
    fn stop(self) {
        let PublicRpc { server, runtime } = self;
        server.stop().ok();
        let stopped = runtime
            .handle()
            .block_on(async { tokio::time::timeout(RPC_STOP_TIMEOUT, server.stopped()).await });
        if stopped.is_err() {
            tracing::warn!("the brc20-prog RPC server did not stop within {RPC_STOP_TIMEOUT:?}");
        }
        drop(runtime);
    }
}

/// Builds a brc20 module on a fresh database for every start and restart.
/// Every module shares the one engine and its Bitcoin RPC source.
fn brc20_factory(engine: Brc20Engine, journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let (engine, journal) = (engine.clone(), journal.clone());
        Box::pin(async move {
            let module = Brc20Module::new(
                Brc20IndexerConfig::default(),
                engine.prog,
                engine.bitcoin_rpc_buffer,
                journal,
            )
            .await?;
            Ok(Box::new(module) as Box<dyn Module>)
        })
    })
}

/// Builds a bitmap module on a fresh database connection for every start and
/// restart.
fn bitmap_factory(config: BitmapConfig, journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let (config, journal) = (config.clone(), journal.clone());
        Box::pin(async move {
            let module = BitmapModule::new(config, journal).await?;
            Ok(Box::new(module) as Box<dyn Module>)
        })
    })
}

/// Builds an sns module on a fresh database connection for every start and
/// restart.
fn sns_factory(config: SnsConfig, journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let (config, journal) = (config.clone(), journal.clone());
        Box::pin(async move {
            let module = SnsModule::new(config, journal).await?;
            Ok(Box::new(module) as Box<dyn Module>)
        })
    })
}

/// Builds a runes module on a fresh database connection for every start and
/// restart.
fn runes_factory(config: RunesConfig, journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let (config, journal) = (config.clone(), journal.clone());
        Box::pin(async move {
            let module = RunesModule::new(config, journal).await?;
            Ok(Box::new(module) as Box<dyn Module>)
        })
    })
}

/// Builds an inscriptions module on a fresh database connection for every
/// start and restart.
fn inscriptions_factory(config: InscriptionsConfig) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let config = config.clone();
        Box::pin(async move {
            let module = InscriptionsModule::new(config).await?;
            Ok(Box::new(module) as Box<dyn Module>)
        })
    })
}

async fn source_finished(handle: &SourceHandle) {
    while !handle.is_finished() {
        tokio::time::sleep(SOURCE_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            Brc20Engine, Cli, Command, PublicRpc, brc20, is_lock_error, open_engine,
            shut_down_brc20,
        },
        bitcoin::{BlockHash, Network, Transaction, Txid},
        brc20_prog::{BitcoinRpc, BitcoinRpcError, Brc20Prog, Brc20ProgConfig},
        clap::Parser,
        opi_brc20::{config::Brc20IndexerConfig, indexer::CachedBitcoinRpc},
        std::{
            io::{Read, Write},
            net::{TcpListener, TcpStream},
            path::PathBuf,
            sync::{Arc, Mutex, MutexGuard},
        },
    };

    /// Serialises the tests that open an engine, since opening one sets
    /// process-wide configuration and the process-wide Bitcoin RPC source.
    static ENGINE_LOCK: Mutex<()> = Mutex::new(());

    /// A Bitcoin RPC source that cannot answer, for tests that never reach
    /// Bitcoin.
    struct NoBitcoin;

    impl BitcoinRpc for NoBitcoin {
        fn get_transaction(&self, _: &Txid) -> Result<Option<Transaction>, BitcoinRpcError> {
            Err("no bitcoind".into())
        }

        fn get_transaction_with_block_hash(
            &self,
            _: &Txid,
        ) -> Result<Option<(Transaction, Option<BlockHash>)>, BitcoinRpcError> {
            Err("no bitcoind".into())
        }

        fn get_block_height(&self, _: &BlockHash) -> Result<usize, BitcoinRpcError> {
            Err("no bitcoind".into())
        }

        fn network(&self) -> Result<Network, BitcoinRpcError> {
            Err("no bitcoind".into())
        }
    }

    fn no_bitcoin() -> Arc<dyn BitcoinRpc> {
        Arc::new(NoBitcoin)
    }

    /// An engine config on a fresh temp directory, removed on drop.
    struct EngineDir {
        config: Brc20ProgConfig,
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl EngineDir {
        fn new() -> Self {
            let guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "opi-indexer-engine-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut config = Brc20ProgConfig::from_env();
            config.db_path = dir.to_string_lossy().into_owned();
            config.bitcoin_rpc_network = "regtest".to_string();
            config.brc20_prog_rpc_server_url = free_address();
            EngineDir {
                config,
                dir,
                _guard: guard,
            }
        }
    }

    impl Drop for EngineDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn free_address() -> String {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string()
    }

    /// The body of the HTTP response to a JSON-RPC call of `method`.
    fn rpc_call(address: &str, method: &str, params: &str) -> String {
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn a_second_open_of_the_same_database_asks_to_stop_run() {
        let dir = EngineDir::new();
        let _first =
            open_engine(dir.config.clone(), no_bitcoin()).expect("the first open succeeds");
        let err = open_engine(dir.config.clone(), no_bitcoin())
            .err()
            .expect("the second open fails");
        assert_eq!(
            err.to_string(),
            format!(
                "brc20-prog's database at {} is in use; stop `opi-indexer run` first",
                dir.config.db_path
            )
        );
    }

    #[test]
    fn other_open_errors_keep_their_message() {
        let dir = EngineDir::new();
        let mut config = dir.config.clone();
        config.db_path = String::new();
        let err = open_engine(config, no_bitcoin())
            .err()
            .expect("an empty path fails");
        assert!(
            err.to_string()
                .starts_with("failed to open the brc20-prog engine: "),
            "{err}"
        );
    }

    /// The engine reads Bitcoin data from the source it opens with, so its
    /// own Bitcoin RPC URL may be empty even when Bitcoin RPC errors are fatal.
    #[test]
    fn the_engine_opens_on_a_source_without_a_bitcoin_rpc_url() {
        let dir = EngineDir::new();
        let mut config = dir.config.clone();
        config.bitcoin_rpc_url = String::new();
        config.bitcoin_rpc_user = String::new();
        config.bitcoin_rpc_password = String::new();
        config.fail_on_bitcoin_rpc_error = true;
        assert!(open_engine(config, no_bitcoin()).is_ok());
    }

    #[test]
    fn lock_errors_are_recognised_by_rocksdb_text() {
        assert!(is_lock_error(
            "IO error: While lock file: /db/LOCK: Resource temporarily unavailable"
        ));
        assert!(is_lock_error(
            "IO error: lock hold by current process, acquire time 1 acquiring thread 2: /db/LOCK: No locks available"
        ));
        assert!(is_lock_error(
            "IO error: While lock file: /db/LOCK: Permission denied"
        ));
        assert!(!is_lock_error(
            "IO error: While lock file: /db/LOCK: No locks available"
        ));
        assert!(!is_lock_error("Invalid block number"));
        assert!(!is_lock_error("failed to read block 5"));
    }

    #[test]
    fn the_public_rpc_server_answers_on_its_own_runtime_and_rejects_indexer_methods() {
        let dir = EngineDir::new();
        let prog: Brc20Prog = open_engine(dir.config.clone(), no_bitcoin()).unwrap();
        // No async runtime drives this thread; only the service runtime can answer.
        let rpc = PublicRpc::start(&prog).expect("the server starts");
        let address = &dir.config.brc20_prog_rpc_server_url;

        let height = rpc_call(address, "eth_blockNumber", "[]");
        assert!(height.contains(r#""result":"0x"#), "{height}");

        let mined = rpc_call(address, "brc20_mine", "[1,0]");
        assert!(mined.contains(r#""error""#), "{mined}");
        assert!(!mined.contains(r#""result""#), "{mined}");

        rpc.stop();
        assert!(TcpStream::connect(address).is_err());
    }

    /// `brc20 light` serves the public JSON-RPC under `run`'s rule, on a
    /// light engine that reads Bitcoin data from the cache only, and stops it
    /// as `run` does; the maintenance commands run none.
    #[test]
    fn brc20_light_serves_the_public_rpc_under_runs_rule() {
        let dir = EngineDir::new();
        let mut config = Brc20IndexerConfig {
            light_client_mode: true,
            bitcoin_rpc_cache_enabled: true,
            network_type: Network::Regtest,
            network_type_string: "regtest".to_string(),
            brc20_prog_rpc_server_enabled: true,
            ..Default::default()
        };
        let source = CachedBitcoinRpc::new(&config).expect("the source starts");
        let engine = Brc20Engine {
            bitcoin_rpc_buffer: source.buffer_slot().clone(),
            prog: open_engine(dir.config.clone(), Arc::new(source)).unwrap(),
        };
        let address = &dir.config.brc20_prog_rpc_server_url;

        for maintenance in [
            brc20_command(&["reorg", "5", "--yes"]),
            brc20_command(&["reindex-extras", "--yes"]),
            brc20_command(&["trace-str", "9"]),
        ] {
            assert!(
                brc20::public_rpc(&maintenance, &engine, &config)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(TcpStream::connect(address).is_err());

        let rpc = brc20::public_rpc(&brc20::Command::Light, &engine, &config)
            .unwrap()
            .expect("light serves the public RPC");
        let height = rpc_call(address, "eth_blockNumber", "[]");
        assert!(height.contains(r#""result":"0x"#), "{height}");

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        shut_down_brc20(&runtime, Some(rpc));
        assert!(TcpStream::connect(address).is_err());

        config.brc20_prog_rpc_server_enabled = false;
        assert!(
            brc20::public_rpc(&brc20::Command::Light, &engine, &config)
                .unwrap()
                .is_none()
        );
    }

    fn brc20_command(args: &[&str]) -> brc20::Command {
        let argv = ["opi-indexer", "brc20"]
            .into_iter()
            .chain(args.iter().copied());
        match Cli::try_parse_from(argv).unwrap().command {
            Some(Command::Brc20(args)) => args.command,
            _ => panic!("not a brc20 command"),
        }
    }

    fn brc20_fails(args: &[&str]) -> bool {
        let argv = ["opi-indexer", "brc20"]
            .into_iter()
            .chain(args.iter().copied());
        Cli::try_parse_from(argv).is_err()
    }

    #[test]
    fn brc20_subcommands_parse() {
        assert!(matches!(brc20_command(&["light"]), brc20::Command::Light));
        assert!(matches!(
            brc20_command(&["reset"]),
            brc20::Command::Reset(brc20::Confirm { yes: false })
        ));
        assert!(matches!(
            brc20_command(&["reset", "--yes"]),
            brc20::Command::Reset(brc20::Confirm { yes: true })
        ));
        assert!(matches!(
            brc20_command(&["reorg", "840000"]),
            brc20::Command::Reorg {
                height: 840000,
                confirm: brc20::Confirm { yes: false }
            }
        ));
        assert!(matches!(
            brc20_command(&["reorg", "840000", "--yes"]),
            brc20::Command::Reorg {
                height: 840000,
                confirm: brc20::Confirm { yes: true }
            }
        ));
        assert!(matches!(
            brc20_command(&["recalculate-hashes", "5"]),
            brc20::Command::RecalculateHashes { from: 5 }
        ));
        assert!(matches!(
            brc20_command(&["reindex-extras", "--yes"]),
            brc20::Command::ReindexExtras(brc20::Confirm { yes: true })
        ));
        assert!(matches!(
            brc20_command(&["reindex-extras"]),
            brc20::Command::ReindexExtras(brc20::Confirm { yes: false })
        ));
        assert!(matches!(
            brc20_command(&["event-str", "9"]),
            brc20::Command::EventStr { height: 9 }
        ));
        assert!(matches!(
            brc20_command(&["trace-str", "9"]),
            brc20::Command::TraceStr { height: 9 }
        ));
    }

    #[test]
    fn brc20_rejects_bad_input_and_removed_flags() {
        assert!(brc20_fails(&[]));
        assert!(brc20_fails(&["reorg"]));
        assert!(brc20_fails(&["reorg", "abc"]));
        assert!(brc20_fails(&["report", "7"]));
        assert!(brc20_fails(&["event-str", "x"]));
        assert!(brc20_fails(&["light", "--yes"]));
        for removed in [
            "--setup",
            "--reset",
            "--validate",
            "--log-level",
            "--report",
        ] {
            assert!(brc20_fails(&[removed]), "{removed}");
        }
    }

    #[test]
    fn confirm_yes_flag_skips_the_prompt() {
        let mut input: &[u8] = b"";
        let mut output = Vec::new();
        assert!(brc20::confirm_with("sure?", true, &mut input, &mut output));
        assert!(output.is_empty());
    }

    #[test]
    fn confirm_accepts_only_yes_answers() {
        for (answer, expected) in [
            ("y\n", true),
            ("YES\n", true),
            (" yes \n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
            ("yep\n", false),
        ] {
            let mut input = answer.as_bytes();
            let mut output = Vec::new();
            assert_eq!(
                brc20::confirm_with("sure?", false, &mut input, &mut output),
                expected,
                "{answer:?}"
            );
            assert_eq!(output, b"sure? [y/N]: ");
        }
    }

    /// This binary links opi-brc20, which turns on serde_json's
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
