mod config;
mod validate;

use {
    anyhow::{Context, Result, bail},
    brc20_index::{config::Brc20IndexerConfig, module::Brc20Module},
    clap::{Parser, Subcommand},
    config::{Config, ModuleKind},
    futures::future::BoxFuture,
    opi_bitmap::{BitmapConfig, BitmapModule},
    opi_core::{BlockSource, Module},
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

/// Every enabled module's configuration, read and checked before the source
/// starts.
struct ModuleConfigs {
    brc20: Option<Brc20IndexerConfig>,
    bitmap: Option<BitmapConfig>,
    sns: Option<SnsConfig>,
}

impl ModuleConfigs {
    fn load(config: &Config) -> Result<ModuleConfigs> {
        let mut configs = ModuleConfigs {
            brc20: None,
            bitmap: None,
            sns: None,
        };
        for module in &config.modules {
            match module {
                ModuleKind::Brc20 => {
                    let brc20 = Brc20IndexerConfig::default();
                    if brc20.network_type != config.network() {
                        bail!(
                            "NETWORK_TYPE {:?} does not match OPI_CHAIN network {:?}",
                            brc20.network_type,
                            config.network()
                        );
                    }
                    configs.brc20 = Some(brc20);
                }
                ModuleKind::Bitmap => {
                    let bitmap = BitmapConfig::from_env().context("bitmap config")?;
                    config.check_network(&bitmap.network_type)?;
                    configs.bitmap = Some(bitmap);
                }
                ModuleKind::Sns => {
                    let sns = SnsConfig::from_env().context("sns config")?;
                    config.check_network(&sns.network_type)?;
                    configs.sns = Some(sns);
                }
            }
        }
        Ok(configs)
    }
}

fn run() -> Result<()> {
    let config = Config::from_env()?;
    let module_configs = ModuleConfigs::load(&config)?;

    // The source blocks on its own runtime, so it starts and stops outside
    // the runtime below.
    let handle = spawn_source(config.source.clone())?;

    let exit = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(supervise(&handle, module_configs)));

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
async fn supervise(handle: &SourceHandle, configs: ModuleConfigs) -> Result<Exit> {
    let journal: Arc<dyn BlockSource> = Arc::new(handle.reader());
    let mut runtime = Runtime::new(journal.clone());
    if configs.brc20.is_some() {
        runtime.add("brc20", brc20_factory(journal.clone()));
    }
    if let Some(config) = configs.bitmap {
        runtime.add("bitmap", bitmap_factory(config, journal.clone()));
    }
    if let Some(config) = configs.sns {
        runtime.add("sns", sns_factory(config, journal));
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

/// Builds a brc20 module on a fresh database for every start and restart.
fn brc20_factory(journal: Arc<dyn BlockSource>) -> ModuleFactory {
    Arc::new(move || -> BoxFuture<'static, Result<Box<dyn Module>>> {
        let journal = journal.clone();
        Box::pin(async move {
            let module = Brc20Module::new(Brc20IndexerConfig::default(), journal).await?;
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

async fn source_finished(handle: &SourceHandle) {
    while !handle.is_finished() {
        tokio::time::sleep(SOURCE_POLL).await;
    }
}

#[cfg(test)]
mod tests {
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
