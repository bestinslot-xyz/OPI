//! Commands that read brc20's environment report bad Bitcoin RPC settings as
//! an error instead of panicking. Each case runs the binary in an empty
//! directory with a cleared environment, so no `.env` file applies.

use std::process::Command;

#[test]
fn bad_bitcoin_rpc_settings_are_an_error_not_a_panic() {
    let dir = std::env::temp_dir().join(format!("opi-indexer-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (args, code) in [
        (&["validate"][..], 2),
        (&["brc20", "reset", "--yes"][..], 1),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_opi-indexer"))
            .args(args)
            .current_dir(&dir)
            .env_clear()
            .env("BITCOIN_RPC_USER", "user")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(
            stderr.contains("BITCOIN_RPC_USER is set without BITCOIN_RPC_PASSWD"),
            "{args:?}: {stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
