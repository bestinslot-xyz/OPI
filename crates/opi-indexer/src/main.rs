mod brc20;
mod config;
mod validate;

use {
    anyhow::{Result, anyhow, bail},
    brc20_prog::{Brc20Prog, Brc20ProgConfig},
    clap::{Parser, Subcommand},
    config::{Config, ModuleConfigs},
    futures::future::BoxFuture,
    jsonrpsee::server::ServerHandle,
    opi_bitmap::{BitmapConfig, BitmapModule},
    opi_brc20::{
        config::Brc20IndexerConfig, fail_stop::RPC_THREAD_NAME, module::Brc20Module,
        service_runtime::ServiceRuntime,
    },
    opi_core::{BlockSource, Module},
    opi_runes::{RunesConfig, RunesModule},
    opi_runtime::{ModuleFactory, Runtime},
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
}

/// Why the modules stopped.
enum Exit {
    Signal,
    SourceEnded,
}

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().map(|()| true),
        Command::Brc20(args) => brc20::run(args).map(|()| true),
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

    // The engine opens once, before anything starts; module restarts reuse it.
    let brc20_prog = module_configs
        .brc20
        .as_ref()
        .map(open_brc20_prog)
        .transpose()?;

    // Serves for the whole process, so brc20 module restarts keep it.
    let public_rpc = match (&brc20_prog, &module_configs.brc20) {
        (Some(brc20_prog), Some(brc20)) if brc20.brc20_prog_rpc_server_enabled => {
            Some(PublicRpc::start(brc20_prog)?)
        }
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

    let exit = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(supervise(&handle, module_configs, brc20_prog)));

    // The modules have stopped; the public RPC server stops before the source.
    if let Some(public_rpc) = public_rpc {
        public_rpc.stop();
    }
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

/// Runs the enabled modules over the journal until a signal arrives or the
/// source driver exits, then stops them.
async fn supervise(
    handle: &SourceHandle,
    configs: ModuleConfigs,
    brc20_prog: Option<Brc20Prog>,
) -> Result<Exit> {
    let journal: Arc<dyn BlockSource> = Arc::new(handle.reader());
    let mut runtime = Runtime::new(journal.clone());
    if let Some(brc20_prog) = brc20_prog {
        runtime.add("brc20", brc20_factory(brc20_prog, journal.clone()));
    }
    if let Some(config) = configs.bitmap {
        runtime.add("bitmap", bitmap_factory(config, journal.clone()));
    }
    if let Some(config) = configs.sns {
        runtime.add("sns", sns_factory(config, journal.clone()));
    }
    if let Some(config) = configs.runes {
        runtime.add("runes", runes_factory(config, journal));
    }

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
        _ = source_finished(handle) => {
            tracing::error!("source driver exited");
            Exit::SourceEnded
        }
    };
    stop.send(()).ok();
    modules.await?;
    Ok(exit)
}

/// Opens the BRC2.0 engine from brc20's environment and brc20-prog's own.
pub(crate) fn open_brc20_prog(config: &Brc20IndexerConfig) -> Result<Brc20Prog> {
    open_engine(config.brc20_prog_config()?)
}

/// Opens the engine at `engine_config.db_path`. RocksDB admits one process
/// per database, so a lock failure means another `opi-indexer` holds it.
fn open_engine(engine_config: Brc20ProgConfig) -> Result<Brc20Prog> {
    let db_path = engine_config.db_path.clone();
    Brc20Prog::open(engine_config).map_err(|e| {
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

/// How long `PublicRpc::stop` waits for in-flight public calls to finish.
const RPC_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The engine's public JSON-RPC server and the runtime it runs on. The
/// runtime has threads of its own, since the engine blocks the thread that
/// calls it.
struct PublicRpc {
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
/// Every module shares the one engine.
fn brc20_factory(brc20_prog: Brc20Prog, journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let (brc20_prog, journal) = (brc20_prog.clone(), journal.clone());
        Box::pin(async move {
            let module =
                Brc20Module::new(Brc20IndexerConfig::default(), brc20_prog, journal).await?;
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

async fn source_finished(handle: &SourceHandle) {
    while !handle.is_finished() {
        tokio::time::sleep(SOURCE_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Cli, Command, PublicRpc, brc20, is_lock_error, open_engine},
        brc20_prog::{Brc20Prog, Brc20ProgConfig},
        clap::Parser,
        std::{
            io::{Read, Write},
            net::{TcpListener, TcpStream},
            path::PathBuf,
            sync::{Mutex, MutexGuard},
        },
    };

    /// Serialises the tests that open an engine, since `Brc20Prog::open` sets
    /// process-wide configuration.
    static ENGINE_LOCK: Mutex<()> = Mutex::new(());

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
            config.bitcoin_rpc_url = "http://127.0.0.1:1".to_string();
            config.bitcoin_rpc_network = "regtest".to_string();
            config.bitcoin_rpc_user = String::new();
            config.bitcoin_rpc_password = String::new();
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
        let _first = open_engine(dir.config.clone()).expect("the first open succeeds");
        let err = open_engine(dir.config.clone())
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
        let err = open_engine(config).err().expect("an empty path fails");
        assert!(
            err.to_string()
                .starts_with("failed to open the brc20-prog engine: "),
            "{err}"
        );
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
        let prog: Brc20Prog = open_engine(dir.config.clone()).unwrap();
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
