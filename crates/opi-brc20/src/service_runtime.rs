use std::time::Duration;

use tokio::runtime::{Builder, Handle, Runtime};

/// A multi-thread tokio runtime on its own OS threads, for services that the
/// engine reaches with blocking calls (the engine's Bitcoin RPC source, the
/// engine's public JSON-RPC server). The indexer awaits engine calls on a
/// single-threaded runtime, so a service that shared that thread could never
/// answer while the engine waits on it.
///
/// Dropping the value stops every task spawned through its handle, without
/// waiting for them, so it is safe to drop inside an async context.
/// `shutdown` waits for the tasks to be dropped instead.
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

    /// Stops every task spawned through its handle and waits up to `timeout`
    /// for the runtime's threads to drop them, so what the tasks hold (such
    /// as the engine and its RocksDB) is released before this returns. It
    /// blocks, so call it outside an async context.
    pub fn shutdown(mut self, timeout: Duration) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(timeout);
        }
    }
}

impl Drop for ServiceRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::ServiceRuntime;

    #[test]
    fn shutdown_drops_what_its_tasks_hold() {
        let runtime = ServiceRuntime::new("service-runtime-test").unwrap();
        let held = Arc::new(());
        let task_held = held.clone();
        let (started, running) = std::sync::mpsc::channel();
        runtime.handle().spawn(async move {
            let _held = task_held;
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        running.recv().unwrap();
        runtime.shutdown(Duration::from_secs(10));
        assert_eq!(Arc::strong_count(&held), 1);
    }
}
