mod server;
use std::{path::PathBuf, sync::Arc};

use rocksdb::{ColumnFamilyDescriptor, DB, Options};
use signal_hook::{consts::SIGINT, iterator::Signals};

use crate::server::start_rpc_server;

enum Chain {
    Mainnet,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

struct Args {
    chain: Chain,
    db_path: Option<PathBuf>,
    api_url: Option<String>,
}

fn parse_args() -> Args {
    let mut chain = Chain::Mainnet;
    let mut db_path = None;
    let mut api_url = None;

    for (idx, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--mainnet" => chain = Chain::Mainnet,
            "--testnet" => chain = Chain::Testnet,
            "--testnet4" => chain = Chain::Testnet4,
            "--signet" => chain = Chain::Signet,
            "--regtest" => chain = Chain::Regtest,
            "--db-path" => {
                if let Some(path) = std::env::args().nth(idx + 1) {
                    db_path = Some(PathBuf::from(path));
                } else {
                    eprintln!("No path provided after --db-path");
                    std::process::exit(1);
                }
            }
            "--api-url" => {
                if let Some(url) = std::env::args().nth(idx + 1) {
                    api_url = Some(url);
                } else {
                    eprintln!("No URL provided after --api-url");
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                println!("Usage: db_reader [OPTIONS]");
                println!("Options:");
                println!("  --mainnet  Use the Mainnet network.");
                println!("  --signet   Use the Signet network.");
                println!("  --testnet  Use the Testnet network.");
                println!("  --testnet4 Use the Testnet4 network.");
                println!("  --regtest  Use the Regtest network.");
                println!("  --db-path <path>  Specify the path to the database.");
                println!(
                    "  --api-url <url>    Specify the API Host and Port to bind to (default: 127.0.0.1:11030)."
                );
                println!("  -h, --help  Show this help message.");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    Args {
        chain,
        db_path,
        api_url,
    }
}

#[tokio::main]
async fn main() {
    rlimit::Resource::NOFILE
        .set(65536, 131072)
        .expect("Failed to set file descriptor limits");
    let mut signals = Signals::new([SIGINT]).expect("Failed to create signal handler");

    let args = parse_args();

    let index_path = if args.db_path.is_some() {
        args.db_path.unwrap().join(PathBuf::from(
            match args.chain {
                Chain::Mainnet => "/mainnet".to_string(),
                Chain::Testnet => "/testnet".to_string(),
                Chain::Testnet4 => "/testnet4".to_string(),
                Chain::Signet => "/signet".to_string(),
                Chain::Regtest => "/regtest".to_string(),
            } + "dbs",
        ))
    } else {
        match args.chain {
            Chain::Mainnet => PathBuf::from("../../../ord/target/release/dbs"),
            Chain::Testnet => PathBuf::from("../../../ord/target/release/testnet/dbs"),
            Chain::Testnet4 => PathBuf::from("../../../ord/target/release/testnet4/dbs"),
            Chain::Signet => PathBuf::from("../../../ord/target/release/signet/dbs"),
            Chain::Regtest => PathBuf::from("../../../ord/target/release/regtest/dbs"),
        }
    };

    let column_families = vec![
        ColumnFamilyDescriptor::new("height_to_block_header", Options::default()),
        ColumnFamilyDescriptor::new("inscription_id_to_sequence_number", Options::default()),
        ColumnFamilyDescriptor::new("sequence_number_to_inscription_entry", Options::default()),
        ColumnFamilyDescriptor::new("outpoint_to_utxo_entry", Options::default()),
        ColumnFamilyDescriptor::new("ord_transfers", Options::default()),
        ColumnFamilyDescriptor::new("ord_inscription_info", Options::default()),
        ColumnFamilyDescriptor::new("ord_index_stats", Options::default()),
    ];

    let db_path = index_path.join("index.db");
    let sec_db_path = index_path.join("secondary.db");
    let db = Arc::new(
        DB::open_cf_descriptors_as_secondary(
            &Options::default(),
            &db_path,
            &sec_db_path,
            column_families,
        )
        .expect("Failed to open database"),
    );

    let rpc_handle = start_rpc_server(&db, &args.api_url).await.unwrap();

    tokio::spawn(rpc_handle.stopped());

    // The server will run indefinitely, handling requests.
    // You can add more functionality or shutdown logic as needed.
    // For now, we just keep the main function running.
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        if signals.pending().next().is_some() {
            println!("Received SIGINT, stopping RPC server...");
            break; // Exit the loop on SIGINT
        }

        db.try_catch_up_with_primary()
            .map_err(|e| eprintln!("Failed to catch up with primary: {}", e))
            .ok();
    }

    println!("RPC server stopped.");
}
