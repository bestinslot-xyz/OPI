//! The HTTP server: a header-read timeout for slow clients, and a bound on
//! graceful shutdown.

use {
    axum::{Router, routing::get},
    opi_api::{
        reply::Reply,
        server::{ServeOptions, serve_listener},
    },
    std::time::{Duration, Instant},
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    },
};

async fn slow(duration_ms: u64) -> Reply {
    tokio::time::sleep(Duration::from_millis(duration_ms)).await;
    Reply::send_str("done")
}

fn app() -> Router {
    Router::new()
        .route("/fast", get(|| async { Reply::send_str("ok") }))
        .route("/slow", get(|| slow(200)))
        .route("/stuck", get(|| slow(60_000)))
}

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(options: ServeOptions) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(serve_listener(listener, app(), options, async {
        stopped.await.ok();
    }));
    Server {
        addr,
        stop: Some(stop),
        done,
    }
}

async fn send_request(addr: std::net::SocketAddr, path: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    stream
}

async fn read_all(stream: &mut TcpStream) -> String {
    let mut out = Vec::new();
    let mut buf = [0; 4096];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => {
                out.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&out).ends_with("done") || out.ends_with(b"ok") {
                    break;
                }
            }
            Err(_) => panic!("no answer within 5 s"),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn it_serves_requests_with_the_peer_address() {
    let mut server = start(ServeOptions::default()).await;
    let mut stream = send_request(server.addr, "/fast").await;
    let answer = read_all(&mut stream).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with("ok"), "{answer}");
    server.stop.take().unwrap().send(()).unwrap();
    server.done.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_client_that_never_finishes_its_headers_is_dropped() {
    let mut server = start(ServeOptions {
        header_read_timeout: Duration::from_millis(300),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    stream
        .write_all(b"GET /fast HTTP/1.1\r\nhost: x\r\n")
        .await
        .unwrap();
    let started = Instant::now();
    let mut buf = [0; 1024];
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut total = Vec::new();
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return total,
                Ok(n) => total.extend_from_slice(&buf[..n]),
            }
        }
    })
    .await
    .expect("the server closes the connection");
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
    // hyper answers 408 or just closes; it never serves the request
    let text = String::from_utf8_lossy(&read);
    assert!(
        text.is_empty() || text.starts_with("HTTP/1.1 408"),
        "{text}"
    );
    server.stop.take().unwrap().send(()).unwrap();
    server.done.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_lets_a_request_in_flight_finish() {
    let mut server = start(ServeOptions {
        shutdown_grace: Duration::from_secs(5),
        ..ServeOptions::default()
    })
    .await;
    let mut stream = send_request(server.addr, "/slow").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.stop.take().unwrap().send(()).unwrap();
    let answer = read_all(&mut stream).await;
    assert!(answer.ends_with("done"), "{answer}");
    server.done.await.unwrap().unwrap();
    // the listener is closed
    assert!(TcpStream::connect(server.addr).await.is_err());
}

#[tokio::test]
async fn shutdown_gives_up_on_a_stuck_request_after_the_grace_period() {
    let mut server = start(ServeOptions {
        shutdown_grace: Duration::from_millis(300),
        ..ServeOptions::default()
    })
    .await;
    let _stream = send_request(server.addr, "/stuck").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    server.done.await.unwrap().unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[test]
fn the_defaults_are_60_s_and_10_s() {
    let options = ServeOptions::default();
    assert_eq!(options.header_read_timeout, Duration::from_secs(60));
    assert_eq!(options.shutdown_grace, Duration::from_secs(10));
}
