mod config;
mod database;
mod indexer;
mod types;

use std::error::Error;

use indexer::Brc20Indexer;
use tracing::Level;

struct Args {
    is_setup: bool,
    is_reset: bool,
}

fn parse_args() -> Args {
    let mut is_setup = false;
    let mut is_reset = false;

    for (idx, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--setup" => is_setup = true,
            "--reset" => is_reset = true,
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
                        _ => eprintln!("Invalid log level: {}", level),
                    }
                } else {
                    eprintln!("No log level provided after --level");
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
                println!("  --help    Show this help message.");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    Args { is_setup, is_reset }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    let args = parse_args();
    if args.is_setup {
        // TODO - Implement setup logic
        return Ok(());
    }
    let mut brc20_indexer = Brc20Indexer::new(Default::default());
    if args.is_reset {
        brc20_indexer.reset().await?;
        return Ok(());
    }
    brc20_indexer
        .run()
        .await
        .expect("Error running the indexer");

    Ok(())
}
