//! The metrics listener and the API's request metrics.
//!
//! The listener serves `GET /metrics` (the process metrics of
//! [`opi_core::metrics::global`]), `GET /health/live` (200 once the process
//! serves) and `GET /health/ready` (200 when its [`Readiness`] check passes,
//! else 503 with the reason). It runs on its own address, apart from the
//! public API, for both `opi-indexer run` and `opi-indexer api`.
//!
//! [`track`] records each API request under its route pattern, never the
//! raw path, and the module whose prefix the route is under.

use {
    crate::{
        config::{MODULES, Module},
        db::Db,
        server::{ServeOptions, serve_listener},
    },
    anyhow::{Context, Result},
    axum::{
        Router,
        extract::{MatchedPath, Request, State},
        http::{StatusCode, header},
        middleware::Next,
        response::{IntoResponse, Response},
        routing::get,
    },
    futures::future::BoxFuture,
    opi_core::metrics::{CONTENT_TYPE, PoolStats, global},
    std::{future::Future, sync::Arc, time::Duration},
    tokio::{net::TcpListener, time::Instant},
};

/// The route label of a request no route matched.
pub const UNMATCHED: &str = "unmatched";

/// The module label of a route outside every module's prefix.
pub const NO_MODULE: &str = "api";

/// How long one module's database has to answer the API's readiness check.
const DB_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// The check behind `/health/ready`: `Ok` when ready, else the reason.
pub type Readiness = Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// The module label of `path`: the module whose `/v1/<module>` prefix it is
/// under, matched without case, or [`NO_MODULE`].
pub fn module_label(path: &str) -> &'static str {
    let Some(rest) = path.strip_prefix("/v1/") else {
        return NO_MODULE;
    };
    let segment = rest.split('/').next().unwrap_or_default();
    MODULES
        .into_iter()
        .find(|module| module.name().eq_ignore_ascii_case(segment))
        .map_or(NO_MODULE, Module::name)
}

/// Middleware that counts each request by route pattern and status and
/// observes its latency.
pub async fn track(matched: Option<MatchedPath>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let response = next.run(request).await;
    let route = matched.as_ref().map_or(UNMATCHED, MatchedPath::as_str);
    global().api_request(
        module_label(route),
        route,
        response.status().as_u16(),
        started.elapsed(),
    );
    response
}

/// The readiness of `opi-indexer run` and `brc20 light`: every module within
/// `max_lag` blocks of the chain tip and no start-up fill running (see
/// [`opi_core::metrics::Metrics::indexer_readiness`]).
pub fn indexer_readiness(max_lag: u64) -> Readiness {
    Arc::new(move || Box::pin(async move { global().indexer_readiness(max_lag) }))
}

/// The readiness of `opi-indexer api`: every module's database answers its
/// latest height within 5 seconds.
pub fn api_readiness(dbs: Vec<(Module, Db)>) -> Readiness {
    let dbs = Arc::new(dbs);
    Arc::new(move || {
        let dbs = dbs.clone();
        Box::pin(async move {
            for (module, db) in dbs.iter() {
                match tokio::time::timeout(DB_READY_TIMEOUT, db.latest_block_height(*module)).await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => {
                        return Err(format!("the {} database: {err:#}", module.name()));
                    }
                    Err(_) => {
                        return Err(format!(
                            "the {} database did not answer within {DB_READY_TIMEOUT:?}",
                            module.name()
                        ));
                    }
                }
            }
            Ok(())
        })
    })
}

/// Reports each module's API pool in the process metrics.
pub fn report_pools(dbs: &[(Module, Db)]) {
    for (module, db) in dbs {
        if let Ok(pool) = db.pool() {
            let pool = pool.clone();
            global().set_pool(module.name(), move || PoolStats {
                size: pool.size(),
                idle: pool.num_idle() as u32,
            });
        }
    }
}

/// The metrics listener's routes.
pub fn metrics_router(readiness: Readiness) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/health/live", get(|| async { "live\n" }))
        .route("/health/ready", get(ready).with_state(readiness))
}

async fn metrics() -> Response {
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], global().encode()).into_response()
}

async fn ready(State(readiness): State<Readiness>) -> Response {
    match readiness().await {
        Ok(()) => "ready\n".into_response(),
        Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, format!("{reason}\n")).into_response(),
    }
}

/// Listens on `addr` for the metrics listener.
pub async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening for metrics on {addr}"))
}

/// Serves the metrics listener on `listener` until `shutdown` completes.
pub async fn serve_metrics(
    listener: TcpListener,
    readiness: Readiness,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    tracing::info!("metrics listening on {}", listener.local_addr()?);
    serve_listener(
        listener,
        metrics_router(readiness),
        ServeOptions::default(),
        shutdown,
    )
    .await
    .context("serving metrics")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_labelled_by_their_module_prefix() {
        assert_eq!(module_label("/v1/brc20/ticker_info/{ticker}"), "brc20");
        assert_eq!(module_label("/v1/SNS/{*rest}"), "sns");
        assert_eq!(module_label("/v1/runes"), "runes");
        assert_eq!(module_label("/v1/status"), NO_MODULE);
        assert_eq!(module_label("/v1/brc20x/a"), NO_MODULE);
        assert_eq!(module_label(UNMATCHED), NO_MODULE);
        assert_eq!(module_label("/"), NO_MODULE);
    }
}
