//! The API's shutdown: readiness answers 503 "shutting down" from the
//! shutdown signal on, and the metrics listener then stops. It runs in a
//! test process of its own, since shutting down marks the whole process.

use {
    opi_api::{
        config::{Module, load_config},
        db::Db,
        js::JsValue,
        serve_listeners,
    },
    std::{collections::HashMap, net::SocketAddr, time::Duration},
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    },
};

async fn get(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_answers_503_once_shutdown_begins() {
    let env: HashMap<&str, &str> = [("OPI_MODULES", "brc20"), ("DB_DATABASE", "d")]
        .into_iter()
        .collect();
    let config = load_config(|key| env.get(key).map(|v| v.to_string())).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics = metrics_listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let dbs = vec![(Module::Brc20, Db::fake(|_| Ok(JsValue::Number(1.0))))];
    let done = tokio::spawn(serve_listeners(
        config,
        dbs.clone(),
        listener,
        Some(metrics_listener),
        async {
            stopped.await.ok();
        },
    ));
    assert_eq!(get(metrics, "/health/ready").await.0, "HTTP/1.1 200 OK");

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), done)
        .await
        .expect("the API stops")
        .unwrap()
        .unwrap();
    // The metrics listener has stopped with the API, and the API's
    // readiness answers 503 from the shutdown on.
    assert!(TcpStream::connect(metrics).await.is_err());
    assert_eq!(
        opi_core::metrics::global().phase(),
        opi_core::metrics::Phase::ShuttingDown
    );
    let readiness = opi_api::metrics::api_readiness(dbs);
    assert_eq!(readiness().await, Err("shutting down".to_string()));
}
