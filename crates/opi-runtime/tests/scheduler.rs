//! Drives `Runtime` against an in-memory journal and fake modules that keep
//! their "database" in shared state, so assertions survive module restarts.
//! Every test runs on tokio's paused clock: sleeps advance virtual time
//! instantly once all tasks are idle.

use {
    anyhow::{Result, bail},
    opi_core::{BlockInfo, BlockRef, BlockSource, BlockView, InscriptionFeed, Module},
    opi_runtime::{ModuleFactory, Runtime, RuntimeOptions},
    std::{
        collections::BTreeMap,
        sync::{Arc, Mutex, MutexGuard},
        time::Duration,
    },
    tokio::{
        sync::oneshot,
        task::JoinHandle,
        time::{Instant, sleep, timeout},
    },
};

/// A journal whose finalized blocks are `hashes[height]`.
struct MemSource {
    hashes: Mutex<Vec<String>>,
    /// `(height, tip)`: right after `block_info(height)` answers, the journal
    /// drops every block above `tip`. Fires once.
    truncate_after_info: Mutex<Option<(u32, u32)>>,
}

impl MemSource {
    /// Blocks `0..=tip` with hashes `a-<height>`.
    fn new(tip: u32) -> Arc<Self> {
        let source = Arc::new(Self {
            hashes: Mutex::new(Vec::new()),
            truncate_after_info: Mutex::new(None),
        });
        source.set_chain(0, "a", tip);
        source
    }

    /// Replaces every block from `from` with `<fork>-<height>` up to `tip`.
    fn set_chain(&self, from: u32, fork: &str, tip: u32) {
        let mut hashes = lock(&self.hashes);
        hashes.truncate(from as usize);
        hashes.extend((from..=tip).map(|height| format!("{fork}-{height}")));
    }

    /// Extends the current chain to `tip` with `a-<height>` hashes.
    fn grow(&self, tip: u32) {
        let mut hashes = lock(&self.hashes);
        let from = hashes.len() as u32;
        hashes.extend((from..=tip).map(|height| format!("a-{height}")));
    }

    fn info(&self, height: u32) -> Option<BlockInfo> {
        lock(&self.hashes)
            .get(height as usize)
            .map(|hash| BlockInfo {
                height,
                hash: hash.clone(),
                time: 1_700_000_000 + height,
            })
    }

    fn chain(&self, from: u32) -> BTreeMap<u32, String> {
        lock(&self.hashes)
            .iter()
            .enumerate()
            .skip(from as usize)
            .map(|(height, hash)| (height as u32, hash.clone()))
            .collect()
    }
}

impl BlockSource for MemSource {
    fn tip(&self) -> Result<Option<u32>> {
        Ok((lock(&self.hashes).len() as u32).checked_sub(1))
    }

    fn block_info(&self, height: u32) -> Result<Option<BlockInfo>> {
        let info = self.info(height);
        let mut truncate = lock(&self.truncate_after_info);
        if let Some((at, tip)) = *truncate
            && at == height
        {
            *truncate = None;
            lock(&self.hashes).truncate(tip as usize + 1);
        }
        Ok(info)
    }

    fn inscription_records(&self, _: u32) -> Result<Vec<opi_core::InscriptionRecord>> {
        Ok(Vec::new())
    }

    fn inscription_info(&self, _: &str) -> Result<Option<opi_core::InscriptionInfo>> {
        Ok(None)
    }

    fn block_view(&self, height: u32) -> Result<Option<BlockView>> {
        Ok(self.info(height).map(|info| BlockView {
            height,
            hash: info.hash,
            time: info.time,
            inscriptions: InscriptionFeed {
                records: Vec::new(),
                infos: BTreeMap::new(),
            },
            runes: opi_core::RuneFeed::default(),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Applied {
    instance: usize,
    height: u32,
    hash: String,
    /// Blocks this instance had applied before this one.
    applied_before: usize,
}

/// Shared state of one fake module: its persisted blocks plus a record of
/// every call and lifecycle event.
#[derive(Default)]
struct Store {
    blocks: BTreeMap<u32, String>,
    applied: Vec<Applied>,
    rollbacks: Vec<u32>,
    prunes: Vec<u32>,
    builds: Vec<Instant>,
    drops: usize,
    fail_build: bool,
    /// `apply` of this height panics once.
    panic_at: Option<u32>,
    panicked_at: Option<Instant>,
    /// `apply` waits while the block is above this height.
    hold_above: Option<u32>,
    apply_delay: Duration,
}

type Shared = Arc<Mutex<Store>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct FakeModule {
    name: &'static str,
    start: u32,
    store: Shared,
    instance: usize,
    applied: usize,
}

impl Drop for FakeModule {
    fn drop(&mut self) {
        lock(&self.store).drops += 1;
    }
}

#[async_trait::async_trait]
impl Module for FakeModule {
    fn name(&self) -> &'static str {
        self.name
    }

    fn start_height(&self) -> u32 {
        self.start
    }

    async fn cursor(&mut self) -> Result<Option<BlockRef>> {
        Ok(lock(&self.store)
            .blocks
            .last_key_value()
            .map(|(height, hash)| BlockRef {
                height: *height,
                hash: hash.clone(),
            }))
    }

    async fn block_hash(&mut self, height: u32) -> Result<Option<String>> {
        Ok(lock(&self.store).blocks.get(&height).cloned())
    }

    async fn rollback_to(&mut self, height: u32) -> Result<()> {
        let mut store = lock(&self.store);
        store.rollbacks.push(height);
        store.blocks.split_off(&(height + 1));
        Ok(())
    }

    async fn prune_below(&mut self, height: u32) -> Result<()> {
        lock(&self.store).prunes.push(height);
        Ok(())
    }

    async fn apply(&mut self, block: &BlockView) -> Result<()> {
        while lock(&self.store)
            .hold_above
            .is_some_and(|hold| block.height > hold)
        {
            sleep(Duration::from_millis(1)).await;
        }
        let delay = lock(&self.store).apply_delay;
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let mut store = lock(&self.store);
        if store.panic_at == Some(block.height) {
            store.panic_at = None;
            store.panicked_at = Some(Instant::now());
            drop(store);
            panic!("fake module failed at {}", block.height);
        }
        store.blocks.insert(block.height, block.hash.clone());
        store.applied.push(Applied {
            instance: self.instance,
            height: block.height,
            hash: block.hash.clone(),
            applied_before: self.applied,
        });
        self.applied += 1;
        Ok(())
    }
}

fn factory(name: &'static str, start: u32, store: &Shared) -> ModuleFactory {
    let store = store.clone();
    Arc::new(move || {
        let store = store.clone();
        Box::pin(async move {
            let instance = {
                let mut state = lock(&store);
                state.builds.push(Instant::now());
                if state.fail_build {
                    bail!("database unavailable");
                }
                state.builds.len()
            };
            Ok(Box::new(FakeModule {
                name,
                start,
                store,
                instance,
                applied: 0,
            }) as Box<dyn Module>)
        })
    })
}

fn store() -> Shared {
    Arc::default()
}

fn store_with(blocks: impl IntoIterator<Item = (u32, String)>) -> Shared {
    let store = store();
    lock(&store).blocks = blocks.into_iter().collect();
    store
}

fn fast_options() -> RuntimeOptions {
    RuntimeOptions {
        poll: Duration::from_millis(10),
        min_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(400),
        healthy_reset: Duration::from_secs(1),
        prune: None,
    }
}

struct Running {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

fn start(
    source: &Arc<MemSource>,
    options: RuntimeOptions,
    modules: Vec<(&'static str, ModuleFactory)>,
) -> Running {
    let mut runtime = Runtime::with_options(source.clone(), options);
    for (name, factory) in modules {
        runtime.add(name, factory);
    }
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(runtime.run(async move {
        let _ = stopped.await;
    }));
    Running { stop, task }
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        timeout(Duration::from_secs(1), self.task)
            .await
            .expect("runtime stops promptly")
            .expect("runtime task does not panic")
            .expect("runtime returns Ok");
    }
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3600);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(5)).await;
    }
}

fn cursor_is(store: &Shared, height: u32) -> impl FnMut() -> bool {
    let store = store.clone();
    move || lock(&store).blocks.last_key_value().map(|(h, _)| *h) == Some(height)
}

fn matches_chain(store: &Shared, source: &Arc<MemSource>, from: u32) -> impl FnMut() -> bool {
    let store = store.clone();
    let source = source.clone();
    move || lock(&store).blocks == source.chain(from)
}

fn applied_heights(store: &Shared) -> Vec<u32> {
    lock(store).applied.iter().map(|a| a.height).collect()
}

#[tokio::test(start_paused = true)]
async fn prunes_every_hundred_blocks_when_set() {
    let source = MemSource::new(2500);
    let pruned = store();
    let kept = store();
    let running = start(
        &source,
        RuntimeOptions {
            prune: Some(1000),
            ..fast_options()
        },
        vec![("pruned", factory("pruned", 0, &pruned))],
    );
    wait_until("pruned module at tip", cursor_is(&pruned, 2500)).await;
    running.stop().await;
    let expected = (0..=25)
        .map(|n| (n * 100 + 1u32).saturating_sub(1000))
        .collect::<Vec<_>>();
    assert_eq!(lock(&pruned).prunes, expected);

    let running = start(
        &source,
        fast_options(),
        vec![("kept", factory("kept", 0, &kept))],
    );
    wait_until("kept module at tip", cursor_is(&kept, 2500)).await;
    running.stop().await;
    assert!(lock(&kept).prunes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn applies_blocks_in_order_from_start_height() {
    let source = MemSource::new(10);
    let module = store();
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 3, &module))],
    );

    wait_until("cursor 10", cursor_is(&module, 10)).await;
    assert_eq!(applied_heights(&module), (3..=10).collect::<Vec<_>>());

    source.grow(12);
    wait_until("cursor 12", cursor_is(&module, 12)).await;
    running.stop().await;

    assert_eq!(applied_heights(&module), (3..=12).collect::<Vec<_>>());
    assert_eq!(lock(&module).blocks, source.chain(3));
    assert!(lock(&module).rollbacks.is_empty());
}

#[tokio::test(start_paused = true)]
async fn new_module_replays_from_first_height() {
    let source = MemSource::new(10);
    let caught_up = store_with(source.chain(2));
    let fresh = store();
    let running = start(
        &source,
        fast_options(),
        vec![
            ("caught_up", factory("caught_up", 2, &caught_up)),
            ("fresh", factory("fresh", 2, &fresh)),
        ],
    );

    wait_until("fresh at 10", cursor_is(&fresh, 10)).await;
    running.stop().await;

    assert_eq!(applied_heights(&fresh), (2..=10).collect::<Vec<_>>());
    assert_eq!(lock(&fresh).blocks, source.chain(2));
    assert!(applied_heights(&caught_up).is_empty());
    assert!(lock(&caught_up).rollbacks.is_empty());
}

#[tokio::test(start_paused = true)]
async fn rolls_back_to_agreement_point_on_hash_change() {
    let source = MemSource::new(10);
    let module = store();
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );
    wait_until("cursor 10", cursor_is(&module, 10)).await;

    source.set_chain(8, "b", 11);
    wait_until("reorged chain", matches_chain(&module, &source, 0)).await;
    running.stop().await;

    let store = lock(&module);
    assert_eq!(store.rollbacks, [7]);
    let after: Vec<_> = store.applied[11..]
        .iter()
        .map(|a| (a.height, a.hash.as_str()))
        .collect();
    assert_eq!(after, [(8, "b-8"), (9, "b-9"), (10, "b-10"), (11, "b-11")]);
}

#[tokio::test(start_paused = true)]
async fn waits_while_journal_is_below_cursor() {
    let source = MemSource::new(10);
    let module = store();
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );
    wait_until("cursor 10", cursor_is(&module, 10)).await;
    let before = lock(&module).blocks.clone();

    source.set_chain(0, "a", 5);
    sleep(Duration::from_secs(60)).await;
    assert!(lock(&module).rollbacks.is_empty());
    assert_eq!(lock(&module).blocks, before);

    source.grow(12);
    wait_until("cursor 12", cursor_is(&module, 12)).await;
    running.stop().await;

    assert!(lock(&module).rollbacks.is_empty());
    assert_eq!(applied_heights(&module), (0..=12).collect::<Vec<_>>());
}

#[tokio::test(start_paused = true)]
async fn rolls_back_once_journal_reaches_cursor_with_new_hash() {
    let source = MemSource::new(10);
    let module = store();
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );
    wait_until("cursor 10", cursor_is(&module, 10)).await;
    let before = lock(&module).blocks.clone();

    source.set_chain(8, "b", 9);
    sleep(Duration::from_secs(60)).await;
    assert!(lock(&module).rollbacks.is_empty());
    assert_eq!(lock(&module).blocks, before);

    source.set_chain(8, "b", 11);
    wait_until("reorged chain", matches_chain(&module, &source, 0)).await;
    running.stop().await;

    assert_eq!(lock(&module).rollbacks, [7]);
}

#[tokio::test(start_paused = true)]
async fn rolls_back_below_start_height_when_nothing_agrees() {
    let source = MemSource::new(6);
    let module = store_with((3..=5).map(|height| (height, format!("x-{height}"))));
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 3, &module))],
    );

    wait_until("journal chain", matches_chain(&module, &source, 3)).await;
    running.stop().await;

    assert_eq!(lock(&module).rollbacks, [2]);
    assert_eq!(applied_heights(&module), [3, 4, 5, 6]);
}

#[tokio::test(start_paused = true)]
async fn rollback_of_exactly_the_cap_succeeds() {
    let source = MemSource::new(1100);
    let module = store_with(diverging_from(101, 1100));
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );

    wait_until("journal chain", matches_chain(&module, &source, 0)).await;
    running.stop().await;

    let store = lock(&module);
    assert_eq!(store.rollbacks, [100]);
    assert_eq!(store.builds.len(), 1);
}

/// `a-<height>` hashes below `diverge`, `x-<height>` from it up to `cursor`.
fn diverging_from(diverge: u32, cursor: u32) -> impl Iterator<Item = (u32, String)> {
    (0..=cursor).map(move |height| {
        let fork = if height < diverge { "a" } else { "x" };
        (height, format!("{fork}-{height}"))
    })
}

#[tokio::test(start_paused = true)]
async fn rollback_deeper_than_limit_restarts_instead() {
    let source = MemSource::new(1100);
    let module = store_with(diverging_from(100, 1100));
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );

    let watched = module.clone();
    wait_until("a restart", move || lock(&watched).builds.len() >= 2).await;
    running.stop().await;

    let store = lock(&module);
    assert!(store.rollbacks.is_empty());
    assert!(store.applied.is_empty());
    assert_eq!(store.blocks.len(), 1101);
}

#[tokio::test(start_paused = true)]
async fn modules_at_different_heights_roll_back_independently() {
    let source = MemSource::new(10);
    let ahead = store();
    let behind = store();
    lock(&behind).hold_above = Some(9);
    // Followed another chain from height 5 and sits above the journal's tip,
    // so it waits until the journal reaches its cursor.
    let stale = store_with((0..=12).map(|height| {
        let fork = if height < 5 { "a" } else { "s" };
        (height, format!("{fork}-{height}"))
    }));
    let running = start(
        &source,
        fast_options(),
        vec![
            ("ahead", factory("ahead", 0, &ahead)),
            ("behind", factory("behind", 0, &behind)),
            ("stale", factory("stale", 0, &stale)),
        ],
    );
    wait_until("ahead at 10", cursor_is(&ahead, 10)).await;
    wait_until("behind at 9", cursor_is(&behind, 9)).await;
    assert!(lock(&stale).rollbacks.is_empty());

    source.set_chain(8, "b", 12);
    wait_until("ahead reorged", matches_chain(&ahead, &source, 0)).await;
    wait_until("stale reorged", matches_chain(&stale, &source, 0)).await;
    lock(&behind).hold_above = None;
    wait_until("behind reorged", matches_chain(&behind, &source, 0)).await;
    running.stop().await;

    assert_eq!(lock(&ahead).rollbacks, [7]);
    assert_eq!(lock(&behind).rollbacks, [7]);
    assert_eq!(lock(&stale).rollbacks, [4]);
    assert_eq!(applied_heights(&stale), (5..=12).collect::<Vec<_>>());
}

#[tokio::test(start_paused = true)]
async fn rollback_walk_waits_when_journal_drops_below_it() {
    let source = MemSource::new(10);
    let module = store_with(diverging_from(8, 10));
    *lock(&source.truncate_after_info) = Some((9, 5));
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );

    sleep(Duration::from_secs(60)).await;
    assert_eq!(source.chain(0).len(), 6);
    assert!(lock(&module).rollbacks.is_empty());
    assert_eq!(lock(&module).blocks.len(), 11);

    source.set_chain(0, "a", 12);
    wait_until("journal chain", matches_chain(&module, &source, 0)).await;
    running.stop().await;

    assert_eq!(lock(&module).rollbacks, [7]);
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_backoff_stops_promptly() {
    let source = MemSource::new(10);
    let module = store();
    lock(&module).fail_build = true;
    let options = RuntimeOptions {
        min_backoff: Duration::from_secs(3600),
        ..fast_options()
    };
    let running = start(
        &source,
        options,
        vec![("fake", factory("fake", 0, &module))],
    );

    let watched = module.clone();
    wait_until("first build", move || lock(&watched).builds.len() == 1).await;
    running.stop().await;

    assert_eq!(lock(&module).builds.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn slow_module_does_not_block_others() {
    let source = MemSource::new(10);
    let fast = store();
    let slow = store();
    lock(&slow).apply_delay = Duration::from_secs(3600);
    let running = start(
        &source,
        fast_options(),
        vec![
            ("fast", factory("fast", 0, &fast)),
            ("slow", factory("slow", 0, &slow)),
        ],
    );

    timeout(
        Duration::from_secs(60),
        wait_until("fast at 10", cursor_is(&fast, 10)),
    )
    .await
    .expect("fast module reaches the tip within a minute");
    running.stop().await;

    assert!(applied_heights(&slow).is_empty());
}

#[tokio::test(start_paused = true)]
async fn panic_restarts_with_fresh_instance() {
    let source = MemSource::new(10);
    let module = store();
    lock(&module).panic_at = Some(5);
    let running = start(
        &source,
        fast_options(),
        vec![("fake", factory("fake", 0, &module))],
    );

    wait_until("cursor 10", cursor_is(&module, 10)).await;
    running.stop().await;

    let store = lock(&module);
    assert_eq!(store.builds.len(), 2);
    assert_eq!(store.drops, 2);
    let summary: Vec<_> = store
        .applied
        .iter()
        .map(|a| (a.instance, a.height, a.applied_before))
        .collect();
    let expected: Vec<_> = (0..=4)
        .map(|h| (1, h, h as usize))
        .chain((5..=10).map(|h| (2, h, h as usize - 5)))
        .collect();
    assert_eq!(summary, expected);
}

#[tokio::test(start_paused = true)]
async fn backoff_doubles_to_max_and_resets_after_healthy_run() {
    let source = MemSource::new(3);
    let module = store();
    lock(&module).fail_build = true;
    let running = start(
        &source,
        RuntimeOptions::default(),
        vec![("fake", factory("fake", 0, &module))],
    );

    let watched = module.clone();
    wait_until("eight builds", move || lock(&watched).builds.len() >= 8).await;
    let builds = {
        let mut store = lock(&module);
        store.fail_build = false;
        store.builds.clone()
    };
    let gaps: Vec<u64> = builds.windows(2).map(|w| (w[1] - w[0]).as_secs()).collect();
    assert_eq!(gaps, [5, 10, 20, 40, 80, 160, 300]);

    wait_until("cursor 3", cursor_is(&module, 3)).await;
    sleep(Duration::from_secs(11 * 60)).await;
    lock(&module).panic_at = Some(4);
    source.grow(4);
    wait_until("cursor 4", cursor_is(&module, 4)).await;
    running.stop().await;

    let store = lock(&module);
    assert_eq!(store.builds.len(), 10);
    let backoff = store.builds[9] - store.panicked_at.unwrap();
    assert_eq!(backoff.as_secs(), 5);
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_all_modules() {
    let source = MemSource::new(10);
    let idle = store();
    let busy = store();
    lock(&busy).apply_delay = Duration::from_secs(3600);
    let running = start(
        &source,
        fast_options(),
        vec![
            ("idle", factory("idle", 0, &idle)),
            ("busy", factory("busy", 0, &busy)),
        ],
    );
    wait_until("idle at 10", cursor_is(&idle, 10)).await;

    running.stop().await;
    source.grow(20);
    sleep(Duration::from_secs(7200)).await;

    for store in [&idle, &busy] {
        let store = lock(store);
        assert_eq!(store.builds.len(), 1);
        assert_eq!(store.drops, 1);
    }
    assert_eq!(applied_heights(&idle), (0..=10).collect::<Vec<_>>());
    assert!(applied_heights(&busy).is_empty());
}
