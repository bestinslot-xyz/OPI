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
    database::{Brc20Database, set_brc20_database},
};

struct Args {
    is_setup: bool,
    is_reset: bool,
    report_block_height: Option<i32>,
    reorg_height: Option<i32>,
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
    let mut is_setup = false;
    let mut is_reset = false;
    let mut reorg_height: Option<i32> = None;
    let mut report_block_height: Option<i32> = None;

    for (idx, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--setup" => is_setup = true,
            "--reset" => is_reset = true,
            "--report" => {
                if let Some(height_str) = std::env::args().nth(idx + 1) {
                    if let Ok(height) = height_str.parse::<i32>() {
                        report_block_height = Some(height);
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
                        reorg_height = Some(height);
                    } else {
                        return Err("Invalid height for --reorg".into());
                    }
                } else {
                    return Err("No height provided after --reorg".into());
                }
            }
            "--log-level" | "-l" => {
                if let Some(level) = std::env::args().nth(idx + 1) {
                    match level.as_str() {
                        "trace" => tracing::subscriber::set_global_default(
                            tracing_subscriber::fmt()
                                .with_target(false)
                                .with_max_level(Level::TRACE)
                                .finish(),
                        )
                        .expect("Failed to set global subscriber"),
                        "debug" => tracing::subscriber::set_global_default(
                            tracing_subscriber::fmt()
                                .with_target(false)
                                .with_max_level(Level::DEBUG)
                                .finish(),
                        )
                        .expect("Failed to set global subscriber"),
                        "info" => tracing::subscriber::set_global_default(
                            tracing_subscriber::fmt()
                                .with_target(false)
                                .with_max_level(Level::INFO)
                                .finish(),
                        )
                        .expect("Failed to set global subscriber"),
                        "warn" => tracing::subscriber::set_global_default(
                            tracing_subscriber::fmt()
                                .with_target(false)
                                .with_max_level(Level::WARN)
                                .finish(),
                        )
                        .expect("Failed to set global subscriber"),
                        "error" => tracing::subscriber::set_global_default(
                            tracing_subscriber::fmt()
                                .with_target(false)
                                .with_max_level(Level::ERROR)
                                .finish(),
                        )
                        .expect("Failed to set global subscriber"),
                        _ => {
                            return Err(
                                "Invalid log level. Use trace, debug, info, warn, or error.".into(),
                            );
                        }
                    }
                } else {
                    return Err("No log level provided after --level".into());
                }
            }
            "--help" | "-h" => {
                println!("Usage: brc20_indexer [--setup] [--reset]");
                println!("Options:");
                println!("  --setup   Set up the config and env file.");
                println!("  --reset   Reset the database to its initial state.");
                println!(
                    "  --log-level, -l <level>  Set the log level (trace, debug, info, warn, error)."
                );
                println!("  --reorg <height>  Reorganize the indexer to the specified height.");
                println!("  --help    Show this help message.");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    Ok(Args {
        is_setup,
        is_reset,
        reorg_height,
        report_block_height,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    let args = parse_args()?;
    if args.is_setup {
        // TODO - Implement setup logic
        return Ok(());
    }
    let config = Brc20IndexerConfig::default();
    let non_interactive = config.non_interactive;
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(&config))));
    let mut brc20_indexer = Brc20Indexer::new(config);
    if let Some(report_height) = args.report_block_height {
        tracing::info!("Reporting block at height {}", report_height);
        brc20_indexer.report_block(report_height).await?;
        return Ok(());
    }
    if let Some(reorg_height) = args.reorg_height {
        if non_interactive || confirm(
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
    if args.is_reset {
        if non_interactive || confirm(
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
