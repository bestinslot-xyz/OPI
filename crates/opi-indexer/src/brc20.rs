//! `opi-indexer brc20`: the BRC-20 light client and maintenance commands.
//!
//! These commands read only brc20's own environment
//! (`Brc20IndexerConfig::default()`). They never load the shared `Config`
//! or open the journal, so they run on a light-client host with no `OPI_*`
//! settings. `event-str`, `trace-str` and `report` run alongside a running
//! `opi-indexer run`; `run` must be stopped before `reset`, `reorg`,
//! `recalculate-hashes` and `reindex-extras`, which rewrite brc20 state.

use {
    anyhow::{Result, anyhow, bail},
    clap::{Args as ClapArgs, Subcommand},
    opi_brc20::{
        config::Brc20IndexerConfig,
        database::{Brc20Database, get_brc20_database, set_brc20_database},
        indexer::Brc20Indexer,
    },
    std::{
        io::{self, BufRead, Write},
        sync::Arc,
    },
    tokio::sync::Mutex,
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
    /// Report the BRC-20 data of a block height to the OPI network.
    Report {
        /// Block height to report.
        height: i32,
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
    /// Print the block event string at a height.
    EventStr {
        /// Block height.
        height: i32,
    },
    /// Print the block trace string at a height.
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
    let config = Brc20IndexerConfig::default();
    if matches!(args.command, Command::Light) && !config.light_client_mode {
        bail!(
            "`brc20 light` runs the light client; set OPERATION_MODE=light. Full mode indexes inside `opi-indexer run` with OPI_MODULES including brc20."
        );
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(execute(args.command, config))
}

// The library reports `Box<dyn Error>`, which is not `Send`; `anyhow!`
// keeps the message.
async fn execute(command: Command, config: Brc20IndexerConfig) -> Result<()> {
    set_brc20_database(Arc::new(Mutex::new(Brc20Database::new(&config))));
    let brc20_prog = crate::open_brc20_prog(&config)?;
    let mut indexer = Brc20Indexer::new(config, brc20_prog, None);
    match command {
        Command::Light => indexer.run().await.map_err(|e| anyhow!("{e}"))?,
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
        Command::Report { height } => {
            tracing::info!("Reporting block at height {height}");
            indexer
                .report_block(height)
                .await
                .map_err(|e| anyhow!("{e}"))?;
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
        Command::EventStr { height } => {
            match indexer
                .get_block_event_string(height)
                .await
                .map_err(|e| anyhow!("{e}"))?
            {
                Some(text) => println!("Block Event String at height {height}:\n{text}"),
                None => println!("No events found at height {height}"),
            }
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
