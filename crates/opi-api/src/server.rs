//! The HTTP server: hyper's HTTP/1 and HTTP/2 connection handling, a
//! header-read timeout for clients that open a connection and never finish
//! a request, and a graceful shutdown bounded in time.

use {
    axum::{Router, body::Body, extract::ConnectInfo},
    hyper::{Request, body::Incoming, service::service_fn},
    hyper_util::{
        rt::{TokioExecutor, TokioIo, TokioTimer},
        server::{conn::auto::Builder, graceful::GracefulShutdown},
    },
    std::{future::Future, time::Duration},
    tokio::net::TcpListener,
    tower::ServiceExt,
};

pub struct ServeOptions {
    /// How long a client has to send a request's headers.
    pub header_read_timeout: Duration,
    /// How long requests in flight may run on after the shutdown signal;
    /// the server then stops with them unanswered.
    pub shutdown_grace: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            header_read_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

/// Serves `app` on `listener` until `shutdown` completes, then stops
/// accepting, lets open connections finish for up to
/// `options.shutdown_grace`, and returns. Each request carries its peer
/// address as `ConnectInfo<SocketAddr>`.
pub async fn serve_listener(
    listener: TcpListener,
    app: Router,
    options: ServeOptions,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout);
    let graceful = GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    // a full file table, say; the next accept may succeed
                    tracing::warn!("accepting a connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let app = app.clone();
        let service = service_fn(move |mut request: Request<Incoming>| {
            request.extensions_mut().insert(ConnectInfo(peer));
            app.clone().oneshot(request.map(Body::new))
        });
        let connection = builder
            .serve_connection_with_upgrades(TokioIo::new(stream), service)
            .into_owned();
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!("connection from {peer}: {err}");
            }
        });
    }
    drop(listener);
    if tokio::time::timeout(options.shutdown_grace, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!(
            "connections still open {:?} after the shutdown signal; stopping without them",
            options.shutdown_grace
        );
    }
    Ok(())
}
