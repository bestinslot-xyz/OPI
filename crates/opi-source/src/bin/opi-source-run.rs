//! Runs a source from command-line arguments until SIGTERM or SIGINT, then
//! shuts it down cleanly. Prints `tip <height>` to stdout each time the
//! journal tip changes.

use {
    anyhow::{Context, Result},
    clap::{Arg, Command},
    opi_core::BlockSource,
    opi_source::{
        driver::{DEFAULT_CHUNK, SourceConfig, spawn_source},
        proxy::UpstreamAuth,
    },
    std::{io::Write, path::PathBuf, time::Duration},
    tokio::signal::unix::{SignalKind, signal},
};

const TIP_POLL: Duration = Duration::from_millis(100);

fn config() -> Result<SourceConfig> {
    let matches = Command::new("opi-source-run")
        .arg(Arg::new("chain").long("chain").default_value("regtest"))
        .arg(Arg::new("ord-data-dir").long("ord-data-dir").required(true))
        .arg(Arg::new("journal-dir").long("journal-dir").required(true))
        .arg(
            Arg::new("bitcoin-rpc-url")
                .long("bitcoin-rpc-url")
                .required(true),
        )
        .arg(Arg::new("rpc-user").long("rpc-user").requires("rpc-pass"))
        .arg(Arg::new("rpc-pass").long("rpc-pass").requires("rpc-user"))
        .arg(
            Arg::new("cookie-file")
                .long("cookie-file")
                .conflicts_with("rpc-user"),
        )
        .arg(Arg::new("chunk").long("chunk"))
        .arg(
            Arg::new("no-runes")
                .long("no-runes")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();

    let get = |name: &str| matches.get_one::<String>(name).cloned();
    let bitcoin_auth = match (get("rpc-user"), get("rpc-pass"), get("cookie-file")) {
        (Some(user), Some(pass), None) => UpstreamAuth::UserPass(user, pass),
        (None, None, Some(cookie)) => UpstreamAuth::Cookie(PathBuf::from(cookie)),
        _ => anyhow::bail!("pass either --rpc-user and --rpc-pass, or --cookie-file"),
    };
    Ok(SourceConfig {
        chain: get("chain")
            .unwrap()
            .parse()
            .map_err(|err| anyhow::anyhow!("invalid --chain: {err}"))?,
        ord_data_dir: get("ord-data-dir").unwrap().into(),
        journal_dir: get("journal-dir").unwrap().into(),
        bitcoin_rpc_url: get("bitcoin-rpc-url").unwrap(),
        bitcoin_auth,
        chunk: match get("chunk") {
            Some(chunk) => chunk.parse().context("invalid --chunk")?,
            None => DEFAULT_CHUNK,
        },
        runes: !matches.get_flag("no-runes"),
        journal: Default::default(),
    })
}

fn run() -> Result<()> {
    let source = spawn_source(config()?)?;
    let reader = source.reader();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building signal runtime")?;
    runtime.block_on(async {
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let mut last = None;
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = tokio::time::sleep(TIP_POLL) => {
                    let tip = reader.tip()?;
                    if tip != last {
                        last = tip;
                        if let Some(tip) = tip {
                            let mut stdout = std::io::stdout().lock();
                            writeln!(stdout, "tip {tip}")?;
                            stdout.flush()?;
                        }
                    }
                }
            }
        }
        anyhow::Ok(())
    })?;

    source.shutdown();
    source.join()
}

fn main() {
    if let Err(err) = run() {
        eprintln!("opi-source-run: {err:#}");
        std::process::exit(1);
    }
}
