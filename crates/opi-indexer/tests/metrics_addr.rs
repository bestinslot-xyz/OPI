//! A taken METRICS_ADDR fails `run`'s start with a clear error, before the
//! source starts. The binary runs in an empty directory with a cleared
//! environment, so no `.env` file applies.

use std::{net::TcpListener, process::Command};

#[test]
fn a_taken_metrics_address_fails_the_start() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("opi-indexer-metrics-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_opi-indexer"))
        .arg("run")
        .current_dir(&dir)
        .env_clear()
        .envs([
            ("OPI_CHAIN", "signet"),
            ("NETWORK_TYPE", "signet"),
            ("OPI_ORD_DATA_DIR", dir.join("ord").to_str().unwrap()),
            ("OPI_JOURNAL_DIR", dir.join("journal").to_str().unwrap()),
            ("BITCOIN_RPC_URL", "http://127.0.0.1:1"),
            ("BITCOIN_RPC_USER", "user"),
            ("BITCOIN_RPC_PASSWD", "pass"),
            ("OPI_MODULES", "bitmap"),
            ("BITMAP_DB_DATABASE", "unused"),
            ("METRICS_ADDR", addr.as_str()),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("listening for metrics on {addr}")),
        "{stderr}"
    );
    // The start stopped before the source: no journal was created.
    assert!(!dir.join("journal").exists(), "{stderr}");
    std::fs::remove_dir_all(&dir).ok();
}
