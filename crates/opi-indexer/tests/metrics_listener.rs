//! The metrics listener of `run` answers liveness while start-up blocks.

use {
    opi_core::metrics::{Phase, ReadyLimits},
    std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        time::Duration,
    },
};

/// The status line and body of a GET of `path`, over a blocking socket.
fn get(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

/// The listener runs on its own thread, so it answers while the calling
/// thread is busy with start-up and runs no async runtime.
#[test]
fn liveness_answers_during_start_up() {
    let metrics = opi_api::metrics::MetricsThread::start(
        TcpListener::bind("127.0.0.1:0").unwrap(),
        opi_api::metrics::indexer_readiness(ReadyLimits::default()),
    )
    .unwrap();
    let addr = metrics.local_addr();
    assert_eq!(opi_core::metrics::global().phase(), Phase::Starting);
    assert_eq!(get(addr, "/health/live").0, "HTTP/1.1 200 OK");
    assert_eq!(
        get(addr, "/health/ready"),
        (
            "HTTP/1.1 503 Service Unavailable".to_string(),
            "starting\n".to_string()
        )
    );
    metrics.stop();
    assert_eq!(opi_core::metrics::global().phase(), Phase::ShuttingDown);
    assert!(TcpStream::connect(addr).is_err());
}
