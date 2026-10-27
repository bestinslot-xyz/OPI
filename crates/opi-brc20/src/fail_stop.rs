//! Fail-stop for panics where the engine can be mid-write.
//!
//! A panic on the brc20 thread or on a thread of the runtime that serves the
//! engine's public JSON-RPC can leave the shared engine half-written, with
//! its lock poisoned. The process aborts there instead of unwinding, as the
//! standalone brc20-prog does (it builds with `panic = "abort"`), and a
//! supervisor restarts it.

/// The name of the thread brc20 runs on.
pub const BRC20_THREAD_NAME: &str = "brc20";

/// The name of the threads of the runtime that serves the engine's public
/// JSON-RPC.
pub const RPC_THREAD_NAME: &str = "opi-rpc";

/// Whether a panic on the thread named `thread_name` aborts the process.
pub fn aborts_on_panic(thread_name: Option<&str>) -> bool {
    matches!(thread_name, Some(BRC20_THREAD_NAME | RPC_THREAD_NAME))
}

/// Installs a panic hook that runs the previous hook, so the panic is
/// reported, and then aborts when `aborts_on_panic` holds for the panicking
/// thread.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if aborts_on_panic(std::thread::current().name()) {
            std::process::abort();
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_brc20_and_rpc_threads_abort() {
        assert!(aborts_on_panic(Some(BRC20_THREAD_NAME)));
        assert!(aborts_on_panic(Some(RPC_THREAD_NAME)));
        assert!(aborts_on_panic(Some("brc20")));
        assert!(aborts_on_panic(Some("opi-rpc")));
        for other in [
            None,
            Some("main"),
            Some("tokio-runtime-worker"),
            Some("brc20-services"),
            Some("opi-rpc-test"),
            Some(""),
        ] {
            assert!(!aborts_on_panic(other), "{other:?}");
        }
    }
}
