//! The OPI HTTP API (`opi-indexer api`): the `/v1/...` routes of every
//! enabled module, in Express-compatible form (statuses, bodies, content
//! types and routing rules).
//!
//! A route module adds its routes to an axum [`Router`]; handlers read the
//! request through [`extract`], query their module's [`db::Db`], work on
//! [`js`] values with JavaScript semantics and answer a [`reply::Reply`] or
//! a [`reply::ApiError`] (the 500 answer).

pub mod client_ip;
pub mod config;
pub mod db;
pub mod extract;
mod front;
pub mod js;
pub mod metrics;
pub mod params;
pub mod pg_json;
pub mod qs;
pub mod rate_limit;
pub mod reply;
pub mod routes;
pub mod server;

pub use front::{FrontDoor, front_door};

use {
    anyhow::{Context, Result},
    axum::Router,
    config::{Config, Module},
    db::Db,
    opi_core::metrics::Phase,
    rate_limit::RateLimiter,
    std::{future::Future, sync::Arc},
};

impl FrontDoor {
    /// The front door `config` sets up: its trusted proxy hops and, when
    /// RATE_LIMIT_ENABLE is `true`, a rate limiter.
    pub fn from_config(config: &Config) -> FrontDoor {
        let rate_limit = &config.rate_limit;
        FrontDoor {
            trusted_hops: config.trusted_hops(),
            limiter: rate_limit
                .enabled
                .then(|| Arc::new(RateLimiter::new(rate_limit.window_ms, rate_limit.max))),
        }
    }
}

/// The whole app for `config`, each enabled module reading from its entry
/// in `dbs` (createApp). Every routed request counts in the process metrics
/// (see [`metrics::instrument`]).
pub fn router(config: &Config, dbs: Vec<(Module, Db)>) -> Router {
    front_door(
        metrics::instrument(routes::routes(&config.modules, dbs)),
        FrontDoor::from_config(config),
    )
}

/// A lazily connecting pool for each enabled module (createPools).
pub fn connect(config: &Config) -> Result<Vec<(Module, Db)>> {
    config
        .dbs
        .iter()
        .map(|(module, db)| {
            Db::connect_lazy(db)
                .with_context(|| format!("{} database", module.name()))
                .map(|db| (*module, db))
        })
        .collect()
}

/// Serves the API on API_HOST:API_PORT until `shutdown` completes, with
/// [`ServeOptions`](server::ServeOptions)' defaults: 60 s to send request
/// headers, and at most 10 s for requests in flight after the signal. When
/// API_METRICS_ADDR is set, the metrics listener serves there too.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    config.validate()?;
    let dbs = connect(&config)?;
    let port = config.listen_port()?;
    let listener = tokio::net::TcpListener::bind((config.api.host.as_str(), port))
        .await
        .with_context(|| format!("listening on {}:{port}", config.api.host))?;
    let metrics_listener = match &config.metrics_addr {
        Some(addr) => Some(metrics::bind(addr).await?),
        None => None,
    };
    serve_listeners(config, dbs, listener, metrics_listener, shutdown).await
}

/// Serves the API for `config` on `listener`, each module reading from its
/// entry in `dbs`, and the metrics listener on `metrics_listener`, until
/// `shutdown` completes. The process phase is running from the start and
/// shutting down from `shutdown` on.
pub async fn serve_listeners(
    config: Config,
    dbs: Vec<(Module, Db)>,
    listener: tokio::net::TcpListener,
    metrics_listener: Option<tokio::net::TcpListener>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    // Read each server's integer input rules now. A database that is down
    // at startup does not hold the server back: its rules are read on the
    // first request that needs them.
    for (module, db) in &dbs {
        let (module, db) = (*module, db.clone());
        tokio::spawn(async move {
            match db.int_rules().await {
                Ok(rules) => tracing::info!("{} database: {rules:?} integer input", module.name()),
                Err(err) => tracing::warn!("{} database: {err:#}", module.name()),
            }
            // a pooler in front of Postgres may drop the startup option
            // that forces custom plans
            match db.plan_cache_mode().await {
                Ok(mode) if mode == db::PLAN_CACHE_MODE => {}
                Ok(mode) => tracing::warn!(
                    "{} database: plan_cache_mode is {mode:?}, not {:?}; rows of queries without a full ORDER BY may list in another order than node-pg's",
                    module.name(),
                    db::PLAN_CACHE_MODE
                ),
                Err(err) => tracing::warn!("{} database: {err:#}", module.name()),
            }
        });
    }
    metrics::report_pools(&dbs);
    // The metrics listener stops gracefully once the API has stopped.
    let metrics_server = metrics_listener.map(|metrics_listener| {
        let readiness = metrics::api_readiness(dbs.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let stopped = async {
                stopped.await.ok();
            };
            if let Err(err) = metrics::serve_metrics(metrics_listener, readiness, stopped).await {
                tracing::error!("{err:#}");
            }
        });
        (stop, task)
    });
    opi_core::metrics::global().set_phase(Phase::Running);
    // Readiness answers 503 from the shutdown signal on.
    let shutdown = async move {
        shutdown.await;
        opi_core::metrics::global().set_phase(Phase::ShuttingDown);
    };
    let app = router(&config, dbs);
    let names: Vec<&str> = config.modules.iter().map(|m| m.name()).collect();
    tracing::info!(
        "opi-api ({}) listening on {}",
        names.join(", "),
        listener.local_addr()?
    );
    let served = server::serve_listener(listener, app, server::ServeOptions::default(), shutdown)
        .await
        .context("serving the API");
    if let Some((stop, task)) = metrics_server {
        stop.send(()).ok();
        task.await.ok();
    }
    served
}
