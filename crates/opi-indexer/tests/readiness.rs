//! `run`'s readiness: a module the runtime holds behind the chain tip makes
//! `/health/ready` answer 503 until it catches up.

use {
    opi_core::{
        BlockInfo, BlockRef, BlockSource, BlockView, InscriptionFeed, InscriptionInfo,
        InscriptionRecord, Module, RuneFeed,
    },
    opi_runtime::{ModuleFactory, Runtime, RuntimeOptions},
    std::{
        collections::BTreeMap,
        net::SocketAddr,
        sync::{Arc, Mutex},
        time::Duration,
    },
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::{Semaphore, oneshot},
    },
};

const TIP: u32 = 10;

fn hash(height: u32) -> String {
    format!("{height:064x}")
}

/// A journal whose every block up to [`TIP`] is final.
struct Journal;

impl BlockSource for Journal {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Ok(Some(TIP))
    }

    fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok((height <= TIP).then(|| BlockInfo {
            height,
            hash: hash(height),
            time: 1_700_000_000 + height,
        }))
    }

    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }

    fn inscription_info(&self, _: &str) -> anyhow::Result<Option<InscriptionInfo>> {
        Ok(None)
    }

    fn block_view(&self, height: u32) -> anyhow::Result<Option<BlockView>> {
        Ok((height <= TIP).then(|| BlockView {
            height,
            hash: hash(height),
            time: 1_700_000_000 + height,
            inscriptions: InscriptionFeed {
                records: Vec::new(),
                infos: BTreeMap::new(),
            },
            runes: RuneFeed::default(),
        }))
    }
}

/// A module at height 4 that applies a block only when `gate` lets it.
struct Held {
    height: Arc<Mutex<u32>>,
    gate: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl Module for Held {
    fn name(&self) -> &'static str {
        "held"
    }

    fn start_height(&self) -> u32 {
        1
    }

    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>> {
        let height = *self.height.lock().unwrap();
        Ok(Some(BlockRef {
            height,
            hash: hash(height),
        }))
    }

    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>> {
        Ok(Some(hash(height)))
    }

    async fn rollback_to(&mut self, height: u32) -> anyhow::Result<()> {
        *self.height.lock().unwrap() = height;
        Ok(())
    }

    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        self.gate.acquire().await?.forget();
        *self.height.lock().unwrap() = block.height;
        Ok(())
    }
}

/// The status line and body of a GET of `path`.
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

/// Polls `/health/ready` until its answer satisfies `done`, for up to 10 s.
async fn ready_until(addr: SocketAddr, done: impl Fn(&str, &str) -> bool) -> (String, String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = get(addr, "/health/ready").await;
        if done(&status, &body) {
            return (status, body);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "last answer: {status} {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_module_held_behind_the_tip_is_not_ready_until_it_catches_up() {
    opi_core::metrics::global().set_chain_tip(TIP);
    let gate = Arc::new(Semaphore::new(0));
    let factory: ModuleFactory = {
        let gate = gate.clone();
        let height = Arc::new(Mutex::new(4));
        Arc::new(move || {
            let module = Held {
                height: height.clone(),
                gate: gate.clone(),
            };
            Box::pin(async move { Ok(Box::new(module) as Box<dyn Module>) })
        })
    };
    let mut runtime = Runtime::with_options(
        Arc::new(Journal),
        RuntimeOptions {
            poll: Duration::from_millis(10),
            ..RuntimeOptions::default()
        },
    );
    runtime.add("held", factory);
    let (stop, stopped) = oneshot::channel::<()>();
    let modules = tokio::spawn(runtime.run(async {
        stopped.await.ok();
    }));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let metrics = tokio::spawn(opi_api::metrics::serve_metrics(
        listener,
        opi_api::metrics::indexer_readiness(2),
        std::future::pending(),
    ));

    let (status, body) = ready_until(addr, |_, body| body.contains("behind")).await;
    assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
    assert!(
        body.contains("held is 6 blocks behind the tip 10"),
        "{body}"
    );
    assert_eq!(get(addr, "/health/live").await.0, "HTTP/1.1 200 OK");
    let (_, text) = get(addr, "/metrics").await;
    assert!(
        text.contains(r#"opi_indexed_height{module="held"} 4"#),
        "{text}"
    );
    assert!(
        text.contains(r#"opi_lag_blocks{module="held"} 6"#),
        "{text}"
    );

    // Within two blocks of the tip the process is ready.
    gate.add_permits(4);
    let (status, _) = ready_until(addr, |status, _| status.contains("200")).await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    let (_, text) = get(addr, "/metrics").await;
    assert!(
        text.contains(r#"opi_indexed_height{module="held"} 8"#),
        "{text}"
    );
    assert!(
        text.contains(r#"opi_block_seconds_count{module="held"} 4"#),
        "{text}"
    );

    stop.send(()).unwrap();
    modules.await.unwrap().unwrap();
    metrics.abort();
}
