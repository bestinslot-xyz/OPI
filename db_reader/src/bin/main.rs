mod server;
use std::{path::PathBuf, sync::Arc};

use rocksdb::{ColumnFamilyDescriptor, DB, Options};
use signal_hook::{consts::SIGINT, iterator::Signals};

use crate::server::start_rpc_server;

#[tokio::main]
async fn main() {
    rlimit::Resource::NOFILE
        .set(4096, 8192)
        .expect("Failed to set file descriptor limits");
    let mut signals = Signals::new([SIGINT]).expect("Failed to create signal handler");

    let index_path = PathBuf::from("../../../ord/target/release/dbs");

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

    let rpc_handle = start_rpc_server(&db).await.unwrap();

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
