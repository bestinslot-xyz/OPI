use std::path::PathBuf;

use bitcoin::Network::{Bitcoin, Regtest, Signet, Testnet, Testnet4};
use db_reader::{Config, start_rpc_server};

fn parse_args() -> Config {
    let mut network = Bitcoin;
    let mut db_path = None;
    let mut api_url = None;

    for (idx, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--mainnet" => network = Bitcoin,
            "--testnet" => network = Testnet,
            "--testnet4" => network = Testnet4,
            "--signet" => network = Signet,
            "--regtest" => network = Regtest,
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

    Config {
        network,
        db_path,
        api_url,
    }
}

#[tokio::main]
async fn main() {
    rlimit::Resource::NOFILE
        .set(65536, 131072)
        .expect("Failed to set NOFILE limit");
    start_rpc_server(parse_args()).await.unwrap_or_else(|err| {
        eprintln!("Error running RPC server: {}", err);
        std::process::exit(1);
    });
}
