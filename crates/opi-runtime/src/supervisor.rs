use {
    crate::scheduler::follow,
    anyhow::{Context, Result},
    futures::{FutureExt, future::BoxFuture},
    opi_core::{BlockSource, Module},
    std::{
        any::Any, collections::HashMap, convert::Infallible, future::Future,
        panic::AssertUnwindSafe, sync::Arc, time::Duration,
    },
    tokio::{task::JoinSet, time::Instant},
};

/// Builds a fresh instance of one module, with its own database connection
/// and empty in-memory state.
pub type ModuleFactory = Arc<dyn Fn() -> BoxFuture<'static, Result<Box<dyn Module>>> + Send + Sync>;

/// Timing of the scheduler and supervisor.
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// How often a module that has caught up checks the journal for a new block.
    pub poll: Duration,
    /// Delay before the first restart after a failure.
    pub min_backoff: Duration,
    /// Cap of the restart delay, which doubles after every failure.
    pub max_backoff: Duration,
    /// A module that ran this long before failing restarts after `min_backoff`.
    pub healthy_reset: Duration,
    /// Blocks of history each module keeps below its cursor; `None` keeps
    /// everything.
    pub prune: Option<u32>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            min_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(300),
            healthy_reset: Duration::from_secs(600),
            prune: None,
        }
    }
}

/// Runs every added module against one journal, each on its own supervised
/// task.
pub struct Runtime {
    source: Arc<dyn BlockSource>,
    options: RuntimeOptions,
    modules: Vec<(&'static str, ModuleFactory)>,
}

impl Runtime {
    pub fn new(source: Arc<dyn BlockSource>) -> Runtime {
        Self::with_options(source, RuntimeOptions::default())
    }

    pub fn with_options(source: Arc<dyn BlockSource>, options: RuntimeOptions) -> Runtime {
        Runtime {
            source,
            options,
            modules: Vec::new(),
        }
    }

    /// Adds a module; `name` labels its log lines and its metrics. The module
    /// counts towards the process's readiness from here on.
    pub fn add(&mut self, name: &'static str, factory: ModuleFactory) {
        opi_core::metrics::global().module(name);
        self.modules.push((name, factory));
    }

    /// Runs every module until `shutdown` resolves; returns when all module
    /// tasks have stopped.
    ///
    /// Module tasks are cancelled at their next await point. A module
    /// cancelled inside `apply` is dropped before its block commits, and it
    /// resumes from its cursor on the next start.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let mut tasks = JoinSet::new();
        let mut names = HashMap::new();
        for (name, factory) in self.modules {
            let handle = tasks.spawn(supervise(
                name,
                factory,
                self.source.clone(),
                self.options.clone(),
            ));
            names.insert(handle.id(), name);
        }
        shutdown.await;
        tracing::info!(modules = ?names.values().collect::<Vec<_>>(), "stopping modules");
        tasks.abort_all();
        while let Some(joined) = tasks.join_next_with_id().await {
            match joined {
                Ok((id, ())) => tracing::info!(module = names[&id], "module stopped"),
                Err(err) if err.is_panic() => tracing::error!(
                    module = names[&err.id()],
                    "module supervisor panicked: {err}"
                ),
                Err(err) => tracing::info!(module = names[&err.id()], "module stopped"),
            }
        }
        Ok(())
    }
}

/// Builds and runs one module until its task is cancelled. An error or a
/// panic drops the instance, logs, and builds a new one after a backoff that
/// doubles from `min_backoff` to `max_backoff`; a run that lasted
/// `healthy_reset` or longer restarts after `min_backoff`.
async fn supervise(
    name: &'static str,
    factory: ModuleFactory,
    source: Arc<dyn BlockSource>,
    options: RuntimeOptions,
) {
    let mut backoff = options.min_backoff;
    loop {
        let started = Instant::now();
        let outcome = AssertUnwindSafe(attempt(name, &factory, &source, &options))
            .catch_unwind()
            .await;
        let err = match outcome {
            Ok(Ok(never)) => match never {},
            Ok(Err(err)) => format!("{err:#}"),
            Err(payload) => format!("panic: {}", panic_message(payload.as_ref())),
        };
        if started.elapsed() >= options.healthy_reset {
            backoff = options.min_backoff;
        }
        tracing::error!(
            module = name,
            "module failed, restarting in {backoff:?}: {err}"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(options.max_backoff);
    }
}

async fn attempt(
    name: &'static str,
    factory: &ModuleFactory,
    source: &Arc<dyn BlockSource>,
    options: &RuntimeOptions,
) -> Result<Infallible> {
    let mut module = factory().await.context("building the module")?;
    tracing::info!(module = name, "module started");
    follow(name, module.as_mut(), source, options.poll, options.prune).await
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string payload".into()
    }
}
