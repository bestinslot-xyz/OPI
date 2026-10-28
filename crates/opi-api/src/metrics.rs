//! The metrics listener and the API's request metrics.
//!
//! The listener serves `GET /metrics` (the process metrics of
//! [`opi_core::metrics::global`]), `GET /health/live` (200 while the listener
//! serves) and `GET /health/ready` (200 when its [`Readiness`] check passes,
//! else 503 with the reason). It runs on its own address, apart from the
//! public API, for `opi-indexer run`, `brc20 light` and `api`. Both readiness
//! checks answer 503 "starting" before the process has started its work and
//! "shutting down" once its shutdown has begun.
//!
//! [`instrument`] records each API request under its route pattern, never
//! the raw path, and the module whose prefix the route is under.

use {
    crate::{
        config::{MODULES, Module},
        db::Db,
        reply::internal_error,
        server::{ServeOptions, serve_listener},
    },
    anyhow::{Context, Result},
    axum::{
        Router,
        extract::{MatchedPath, Request, State},
        http::{StatusCode, header},
        middleware::{self, Next},
        response::{IntoResponse, Response},
        routing::get,
    },
    futures::{FutureExt, future::BoxFuture},
    opi_core::metrics::{CONTENT_TYPE, Phase, PoolStats, ReadyLimits, global},
    std::{future::Future, net::SocketAddr, panic::AssertUnwindSafe, sync::Arc, time::Duration},
    tokio::{net::TcpListener, sync::oneshot, time::Instant},
};

/// The route label of a request no route matched.
pub const UNMATCHED: &str = "unmatched";

/// The module label of a route outside every module's prefix.
pub const NO_MODULE: &str = "api";

/// How long the module databases have, together, to answer the API's
/// readiness check.
const DB_READY_TIMEOUT: Duration = Duration::from_secs(2);

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

/// `router` with request metrics: [`track`] around a panic catcher, so a
/// handler that panics answers the 500 body and counts as a 500.
pub fn instrument(router: Router) -> Router {
    router
        .layer(middleware::from_fn(catch_panic))
        .layer(middleware::from_fn(track))
}

/// Answers the 500 body when the handler panics.
async fn catch_panic(request: Request, next: Next) -> Response {
    match AssertUnwindSafe(next.run(request)).catch_unwind().await {
        Ok(response) => response,
        Err(_) => {
            tracing::error!("a handler panicked");
            internal_error()
        }
    }
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

/// The process phase as a readiness answer.
fn phase_readiness() -> Result<(), String> {
    match global().phase() {
        Phase::Starting => Err("starting".into()),
        Phase::ShuttingDown => Err("shutting down".into()),
        Phase::Running => Ok(()),
    }
}

/// The readiness of `opi-indexer run` and `brc20 light` within `limits` (see
/// [`opi_core::metrics::Metrics::indexer_readiness`]).
pub fn indexer_readiness(limits: ReadyLimits) -> Readiness {
    Arc::new(move || Box::pin(async move { global().indexer_readiness(limits) }))
}

/// The readiness of `opi-indexer api`: every module's database answers its
/// latest height, all asked at once, within 2 seconds. The answer names a
/// database that does not answer; the cause goes to the log.
pub fn api_readiness(dbs: Vec<(Module, Db)>) -> Readiness {
    let dbs = Arc::new(dbs);
    Arc::new(move || {
        let dbs = dbs.clone();
        Box::pin(async move {
            phase_readiness()?;
            let checks = dbs.iter().map(|(module, db)| async move {
                let answer =
                    tokio::time::timeout(DB_READY_TIMEOUT, db.latest_block_height(*module)).await;
                let cause = match answer {
                    Ok(Ok(_)) => return Ok(()),
                    Ok(Err(err)) => format!("{err:#}"),
                    Err(_) => format!("no answer within {DB_READY_TIMEOUT:?}"),
                };
                tracing::warn!("readiness: the {} database: {cause}", module.name());
                Err(format!("the {} database is not answering", module.name()))
            });
            futures::future::join_all(checks)
                .await
                .into_iter()
                .collect::<Result<(), String>>()
        })
    })
}

/// Reports each module's API pool in the process metrics.
pub fn report_pools(dbs: &[(Module, Db)]) {
    for (module, db) in dbs {
        if let Ok(pool) = db.pool() {
            let pool = pool.clone();
            global().set_pool(module.name(), move || {
                PoolStats::new(pool.size(), pool.num_idle())
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

/// Serves the metrics listener on `listener` until `shutdown` completes,
/// then lets open requests finish.
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

/// A metrics listener on a thread of its own, with a current-thread tokio
/// runtime, so it answers while the process's own start-up blocks.
/// [`MetricsThread::stop`], or a drop, begins the process's shutdown and
/// stops the listener gracefully.
pub struct MetricsThread {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MetricsThread {
    /// Serves `listener` with `readiness` from now on.
    pub fn start(listener: std::net::TcpListener, readiness: Readiness) -> Result<MetricsThread> {
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building the metrics runtime")?;
        let (stop, stopped) = oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("opi-metrics".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let served = match TcpListener::from_std(listener) {
                        Ok(listener) => {
                            serve_metrics(listener, readiness, async {
                                stopped.await.ok();
                            })
                            .await
                        }
                        Err(err) => Err(err.into()),
                    };
                    if let Err(err) = served {
                        tracing::error!("{err:#}");
                    }
                });
            })
            .context("spawning the metrics thread")?;
        Ok(MetricsThread {
            addr,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Begins the process's shutdown, so readiness answers 503, then stops
    /// the listener and waits for it.
    pub fn stop(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        global().set_phase(Phase::ShuttingDown);
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

impl Drop for MetricsThread {
    fn drop(&mut self) {
        self.finish();
    }
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
