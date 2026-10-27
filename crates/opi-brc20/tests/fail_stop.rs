//! The panic hook aborts the process on the brc20 and RPC threads and lets
//! other threads unwind. Each case runs in a child copy of this test binary,
//! since an abort ends the process.

use std::process::Command;

use opi_brc20::fail_stop::{BRC20_THREAD_NAME, RPC_THREAD_NAME, install_panic_hook};

const CHILD_THREAD: &str = "OPI_FAIL_STOP_CHILD_THREAD";

/// In a child: installs the hook, panics on a thread with the given name and
/// exits 0 when the panic unwound. In the parent: runs each case.
#[test]
fn panics_abort_only_on_the_brc20_and_rpc_threads() {
    if let Ok(name) = std::env::var(CHILD_THREAD) {
        install_panic_hook();
        let joined = std::thread::Builder::new()
            .name(name)
            .spawn(|| panic!("test panic"))
            .unwrap()
            .join();
        assert!(joined.is_err());
        std::process::exit(0);
    }

    for (name, aborts) in [
        (BRC20_THREAD_NAME, true),
        (RPC_THREAD_NAME, true),
        ("brc20-bitcoin-rpc", false),
    ] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "panics_abort_only_on_the_brc20_and_rpc_threads",
                "--nocapture",
            ])
            .env(CHILD_THREAD, name)
            .status()
            .unwrap();
        if aborts {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(6), "{name}: {status:?}");
        } else {
            assert!(status.success(), "{name}: {status:?}");
        }
    }
}
