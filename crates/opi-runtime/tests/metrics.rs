//! The runtime's metrics: a module whose `follow` fails has no height until
//! it reads its cursor again.

use {
    opi_core::{
        BlockInfo, BlockRef, BlockSource, BlockView, InscriptionFeed, InscriptionInfo,
        InscriptionRecord, Module, RuneFeed,
    },
    opi_runtime::{ModuleFactory, Runtime, RuntimeOptions},
    std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    },
    tokio::sync::oneshot,
};

struct Journal;

impl BlockSource for Journal {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Ok(Some(10))
    }

    fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(Some(BlockInfo {
            height,
            hash: format!("h{height}"),
            time: 0,
        }))
    }

    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }

    fn inscription_info(&self, _: &str) -> anyhow::Result<Option<InscriptionInfo>> {
        Ok(None)
    }

    fn block_view(&self, height: u32) -> anyhow::Result<Option<BlockView>> {
        Ok(Some(BlockView {
            height,
            hash: format!("h{height}"),
            time: 0,
            inscriptions: InscriptionFeed {
                records: Vec::new(),
                infos: BTreeMap::new(),
            },
            runes: RuneFeed::default(),
        }))
    }
}

/// A module at height 5 whose every apply fails.
struct Failing {
    failed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Module for Failing {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn start_height(&self) -> u32 {
        0
    }

    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>> {
        Ok(Some(BlockRef {
            height: 5,
            hash: "h5".into(),
        }))
    }

    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>> {
        Ok(Some(format!("h{height}")))
    }

    async fn rollback_to(&mut self, _: u32) -> anyhow::Result<()> {
        Ok(())
    }

    async fn apply(&mut self, _: &BlockView) -> anyhow::Result<()> {
        self.failed.store(true, Ordering::SeqCst);
        anyhow::bail!("the database went away")
    }
}

fn height() -> Option<u64> {
    opi_core::metrics::global()
        .module_heights()
        .into_iter()
        .find(|(name, _)| *name == "failing")
        .and_then(|(_, height)| height)
}

#[tokio::test]
async fn a_failed_module_has_no_height_while_it_waits_to_restart() {
    let failed = Arc::new(AtomicBool::new(false));
    let factory: ModuleFactory = {
        let failed = failed.clone();
        Arc::new(move || {
            let module = Failing {
                failed: failed.clone(),
            };
            Box::pin(async move { Ok(Box::new(module) as Box<dyn Module>) })
        })
    };
    let mut runtime = Runtime::with_options(
        Arc::new(Journal),
        RuntimeOptions {
            poll: Duration::from_millis(10),
            min_backoff: Duration::from_secs(3600),
            ..RuntimeOptions::default()
        },
    );
    runtime.add("failing", factory);
    let (stop, stopped) = oneshot::channel::<()>();
    let modules = tokio::spawn(runtime.run(async {
        stopped.await.ok();
    }));

    tokio::time::timeout(Duration::from_secs(10), async {
        while !failed.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the module applies a block");
    // The failure clears the height read from the cursor.
    tokio::time::timeout(Duration::from_secs(10), async {
        while height().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the height is cleared");

    stop.send(()).unwrap();
    modules.await.unwrap().unwrap();
}
