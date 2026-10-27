//! The OPI HTTP API (`opi-indexer api`): the routes of the Node (Express) API it
//! replaces, answering with the same statuses, bodies and content types.
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
/// in `dbs` (createApp).
pub fn router(config: &Config, dbs: Vec<(Module, Db)>) -> Router {
    front_door(
        routes::routes(&config.modules, dbs),
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
/// headers, and at most 10 s for requests in flight after the signal.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    config.validate()?;
    let dbs = connect(&config)?;
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
        });
    }
    let app = router(&config, dbs);
    let port = config.listen_port()?;
    let listener = tokio::net::TcpListener::bind((config.api.host.as_str(), port))
        .await
        .with_context(|| format!("listening on {}:{port}", config.api.host))?;
    let names: Vec<&str> = config.modules.iter().map(|m| m.name()).collect();
    tracing::info!(
        "opi-api ({}) listening on {}",
        names.join(", "),
        listener.local_addr()?
    );
    server::serve_listener(listener, app, server::ServeOptions::default(), shutdown)
        .await
        .context("serving the API")
}
