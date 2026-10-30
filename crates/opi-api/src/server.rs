//! The HTTP server: HTTP/1 only, with a
//! header-read timeout on every connection, so a client that connects and
//! sends nothing, or never finishes a request's headers, is dropped; TCP
//! keepalive and, on Linux, a TCP user timeout on every connection, so a
//! peer that vanished without closing (and its stream) is dropped within
//! about a minute; and a graceful shutdown bounded in time.

use {
    axum::{Router, body::Body, extract::ConnectInfo},
    hyper::{Request, body::Incoming, server::conn::http1, service::service_fn},
    hyper_util::{
        rt::{TokioIo, TokioTimer},
        server::graceful::GracefulShutdown,
    },
    socket2::{SockRef, TcpKeepalive},
    std::{future::Future, time::Duration},
    tokio::net::{TcpListener, TcpStream},
    tower::ServiceExt,
};

/// The largest request head (request line and headers) a request may send.
const MAX_HEADER_SIZE: usize = 16 * 1024;
/// The most headers a request may send.
const MAX_HEADERS: usize = 2000;

/// How long a connection is idle before its first keepalive probe.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
/// The time between keepalive probes.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// The unanswered keepalive probes that drop a connection.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const KEEPALIVE_PROBES: u32 = 3;
/// How long sent data may stay unacknowledged before the connection drops
/// (Linux). A peer that reads slowly still acknowledges, so only a vanished
/// one reaches it.
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(60);

/// Sets keepalive and the user timeout on an accepted connection.
fn detect_dead_peers(stream: &TcpStream) -> std::io::Result<()> {
    let socket = SockRef::from(stream);
    let keepalive = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let keepalive = keepalive.with_retries(KEEPALIVE_PROBES);
    socket.set_tcp_keepalive(&keepalive)?;
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    socket.set_tcp_user_timeout(Some(USER_TIMEOUT))?;
    Ok(())
}

pub struct ServeOptions {
    /// How long a client has to send a request's headers, from the moment
    /// the server starts waiting for them (on a new connection, as it is
    /// accepted).
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
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout)
        // node's request head cap: a larger head answers 431 and closes
        .max_header_size(MAX_HEADER_SIZE)
        // node accepts up to 2000 headers
        .max_headers(MAX_HEADERS);
    let graceful = GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    // a full file table, say; the next accept may succeed
                    tracing::warn!("accepting a connection: {err}");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(100)) => continue,
                        () = &mut shutdown => break,
                    }
                }
            },
            () = &mut shutdown => break,
        };
        if let Err(err) = detect_dead_peers(&stream) {
            tracing::debug!("setting keepalive on the connection from {peer}: {err}");
        }
        let app = app.clone();
        let service = service_fn(move |mut request: Request<Incoming>| {
            request.extensions_mut().insert(ConnectInfo(peer));
            app.clone().oneshot(request.map(Body::new))
        });
        let connection = builder.serve_connection(TokioIo::new(stream), service);
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
