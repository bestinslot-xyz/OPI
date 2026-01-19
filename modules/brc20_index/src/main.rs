mod client;
mod config;
mod database;
mod indexer;
mod types;

use std::{
    error::Error,
    io::{self, Write},
    sync::Arc,
};

use indexer::Brc20Indexer;
use tokio::sync::Mutex;
use tracing::Level;

use crate::{
    config::Brc20IndexerConfig,
    database::{Brc20Database, get_brc20_database, set_brc20_database},
};

struct Args {
    is_setup: bool,
    is_reset: bool,
    is_validate: bool,
    get_event_str: Option<i32>,
    get_trace_str: Option<i32>,
    report_block_height: Option<i32>,
    reorg_height: Option<i32>,
    recalculate_hashes_height: Option<i32>,
    reindex_extras: bool,
}

fn confirm(prompt: &str) -> bool {
    print!("{} [y/N]: ", prompt);
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    } else {
        false
    }
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        is_setup: false,
        is_reset: false,
        is_validate: false,
        report_block_height: None,
        reorg_height: None,
        reindex_extras: false,
        get_event_str: None,
        get_trace_str: None,
        recalculate_hashes_height: None,
    };

    let mut log_level = Level::WARN;

    for (idx, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--setup" => args.is_setup = true,
            "--reset" => args.is_reset = true,
            "--validate" => args.is_validate = true,
            "--report" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        args.report_block_height = Some(height);
                    } else {
                        return Err("Invalid height for --report".into());
                    }
                } else {
                    return Err("No height provided after --report".into());
                }
            }
            "--reorg" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        args.reorg_height = Some(height);
                    } else {
                        return Err("Invalid height for --reorg".into());
                    }
                } else {
                    return Err("No height provided after --reorg".into());
                }
            }
            "--recalculate-hashes" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        args.recalculate_hashes_height = Some(height);
                    } else {
                        return Err("Invalid height for --recalculate-hashes".into());
                    }
                } else {
                    return Err("No height provided after --recalculate-hashes".into());
                }
            }
            "--reindex-extras" => {
                args.reindex_extras = true;
            }
            "--log-level" | "-l" => {
                if let Some(level) = std::env::args().nth(idx + 1) {
                    match level.as_str() {
                        "trace" => log_level = Level::TRACE,
                        "debug" => log_level = Level::DEBUG,
                        "info" => log_level = Level::INFO,
                        "warn" => log_level = Level::WARN,
                        "error" => log_level = Level::ERROR,
                        _ => return Err("Invalid log level".into()),
                    }
                } else {
                    return Err("No log level provided after --level".into());
                }
            }
            "--block-event-str" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        args.get_event_str = Some(height);
                    } else {
                        return Err("Invalid height for --block-event-str".into());
                    }
                } else {
                    return Err("No height provided after --block-event-str".into());
                }
            }
            "--block-trace-str" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        args.get_trace_str = Some(height);
                    } else {
                        return Err("Invalid height for --block-trace-str".into());
                    }
                } else {
                    return Err("No height provided after --block-trace-str".into());
                }
            }
            "--version" | "-v" => {
                println!("v{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: brc20_indexer [--setup] [--reset] [--validate] [--report <height>] [--reorg <height>] [--log-level <level>] [--help]"
                );
                println!("Options:");
                println!("  --setup   Set up the config and env file.");
                println!("  --reset   Reset the database to its initial state.");
                println!("  --validate   Validate the indexed data against OPI.");
                println!(
                    "  --report <height>  Report the BRC20 data at the specified block height to OPI."
                );
                println!(
                    "  --log-level, -l <level>  Set the log level (trace, debug, info, warn, error)."
                );
                println!(
                    "  --block-event-str <height>  Get the block event string at the specified height."
                );
                println!(
                    "  --block-trace-str <height>  Get the block trace string at the specified height."
                );
                println!("  --reorg <height>  Reorganize the indexer to the specified height.");
                println!("  --version, -v    Show the version information.");
                println!("  --help    Show this help message.");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_target(false)
            .with_max_level(log_level)
            .finish(),
    )?;

    Ok(args)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    let args = parse_args()?;
    if args.is_setup {
        // TODO - Implement setup logic
        return Ok(());
    }
    println!("BRC20 Indexer v{}", env!("CARGO_PKG_VERSION"));
    let config = Brc20IndexerConfig::default();
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(&config))));
    let mut brc20_indexer = Brc20Indexer::new(config);
    if args.is_validate {
        println!("Validating BRC20 indexer data against OPI...");
        if brc20_indexer.validate().await.is_ok() {
            println!("Validation completed successfully.");
        }
        return Ok(());
    }
    if let Some(recalc_height) = args.recalculate_hashes_height {
        tracing::info!(
            "Recalculating cumulative event hashes from height {} to the latest indexed block...",
            recalc_height
        );
        get_brc20_database()
            .lock()
            .await
            .recalculate_cumulative_event_hashes(recalc_height)
            .await?;
        tracing::info!("Recalculation of cumulative event hashes completed successfully.");
        return Ok(());
    }
    if let Some(event_height) = args.get_event_str {
        tracing::info!("Getting block event string at height {}", event_height);
        if let Some(event_str) = brc20_indexer.get_block_event_string(event_height).await? {
            println!(
                "Block Event String at height {}:\n{}",
                event_height, event_str
            );
        } else {
            println!("No events found at height {}", event_height);
        }
        return Ok(());
    }
    if let Some(trace_height) = args.get_trace_str {
        tracing::info!("Getting block trace string at height {}", trace_height);
        if let Some(trace_str) = brc20_indexer.get_block_trace_string(trace_height).await? {
            println!(
                "Block Trace String at height {}:\n{}",
                trace_height, trace_str
            );
        } else {
            println!("No traces found at height {}", trace_height);
        }
        return Ok(());
    }
    if let Some(report_height) = args.report_block_height {
        tracing::info!("Reporting block at height {}", report_height);
        brc20_indexer.report_block(report_height).await?;
        return Ok(());
    }
    if let Some(reorg_height) = args.reorg_height {
        if confirm(
            "Are you sure you want to reorg the indexer? This will reset the state to the specified height.",
        ) {
            brc20_indexer.reorg(reorg_height).await?;
            tracing::info!("Reorg to height {} completed successfully.", reorg_height);
            return Ok(());
        } else {
            tracing::error!("Reorg cancelled.");
            return Ok(());
        }
    }
    if args.reindex_extras {
        if confirm("Are you sure you want to reindex extra data? This may take a long time.") {
            get_brc20_database()
                .lock()
                .await
                .initial_index_of_extra_tables()
                .await?;
            tracing::info!("Reindexing of extra data completed successfully.");
            return Ok(());
        } else {
            tracing::error!("Reindexing cancelled.");
            return Ok(());
        }
    }
    if args.is_reset {
        if confirm(
            "Are you sure you want to reset the indexer? This will delete all data and start fresh.",
        ) {
            brc20_indexer.reset().await?;
            tracing::info!("Indexer reset successfully.");
            return Ok(());
        } else {
            tracing::error!("Reset cancelled.");
            return Ok(());
        }
    }
    brc20_indexer.run().await?;

    Ok(())
}
