//! The OPI HTTP API (`opi-indexer api`): the routes of the Express app in
//! api/, answering with the same statuses, bodies and content types.
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
pub mod pg_json;
pub mod qs;
pub mod rate_limit;
pub mod reply;
pub mod routes;

pub use front::{FrontDoor, front_door};

use {
    anyhow::{Context, Result},
    axum::Router,
    config::{Config, Module},
    db::Db,
    rate_limit::RateLimiter,
    std::{future::Future, net::SocketAddr, sync::Arc},
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

/// Serves the API on API_HOST:API_PORT until `shutdown` completes.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    config.validate()?;
    let app = router(&config, connect(&config)?);
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
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .context("serving the API")
}
