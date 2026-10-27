mod brc20;
mod config;
mod validate;

use {
    anyhow::{Result, anyhow, bail},
    brc20_prog::Brc20Prog,
    clap::{Parser, Subcommand},
    config::{Config, ModuleConfigs},
    futures::future::BoxFuture,
    opi_bitmap::{BitmapConfig, BitmapModule},
    opi_brc20::{config::Brc20IndexerConfig, module::Brc20Module},
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
    /// environment, so it runs on light-client hosts. `event-str`, `trace-str`
    /// and `report` run alongside `run`; stop `run` before `reset`, `reorg`,
    /// `recalculate-hashes` and `reindex-extras`.
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
    let config = Config::from_env()?;
    let module_configs = ModuleConfigs::load(&config)?;

    // The engine opens once, before anything starts; module restarts reuse it.
    let brc20_prog = module_configs
        .brc20
        .is_some()
        .then(open_brc20_prog)
        .transpose()?;

    // The source blocks on its own runtime, so it starts and stops outside
    // the runtime below.
    let handle = spawn_source(config.source.clone())?;

    let exit = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(supervise(&handle, module_configs, brc20_prog)));

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
pub(crate) fn open_brc20_prog() -> Result<Brc20Prog> {
    let engine_config = Brc20IndexerConfig::default().brc20_prog_config()?;
    Brc20Prog::open(engine_config).map_err(|e| anyhow!("failed to open the brc20-prog engine: {e}"))
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
        super::{Cli, Command, brc20},
        clap::Parser,
    };

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
            brc20_command(&["report", "7"]),
            brc20::Command::Report { height: 7 }
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
        assert!(brc20_fails(&["report", "1.5"]));
        assert!(brc20_fails(&["event-str", "x"]));
        assert!(brc20_fails(&["light", "--yes"]));
        assert!(brc20_fails(&["report", "1", "--yes"]));
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
