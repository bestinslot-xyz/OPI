use tokio::runtime::{Builder, Handle, Runtime};

/// A multi-thread tokio runtime on its own OS threads, for services that the
/// engine reaches with blocking calls (the Bitcoin RPC proxy, the engine's
/// public JSON-RPC server). The indexer awaits engine calls on a
/// single-threaded runtime, so a service that shared that thread could never
/// answer while the engine waits on it.
///
/// Dropping the value stops every task spawned through its handle, without
/// waiting for them, so it is safe to drop inside an async context.
pub struct ServiceRuntime {
    runtime: Option<Runtime>,
}

impl ServiceRuntime {
    /// Starts the runtime; its worker threads carry `name`.
    pub fn new(name: &str) -> std::io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name(name)
            .build()?;
        Ok(ServiceRuntime {
            runtime: Some(runtime),
        })
    }

    /// The handle to spawn services onto.
    pub fn handle(&self) -> Handle {
        self.runtime
            .as_ref()
            .expect("the runtime lives until drop")
            .handle()
            .clone()
    }
}

impl Drop for ServiceRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
