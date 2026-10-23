use {
    bitcoin::{BlockHash, Network, Txid, hashes::Hash},
    opi_source::{
        journal::{Journal, cf, table},
        proxy::{BarrierRequest, GateProxy, ProxyConfig, UpstreamAuth},
        raw_event::RawEvent,
        stager::{Stager, truncate_raw_above},
    },
    ord::{Chain, index::event::Event},
    ordinals::RuneId,
    redb::{Database, ReadableTable},
    serde_json::json,
    std::{
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    },
    tokio::{
        sync::{mpsc, oneshot},
        task::JoinHandle,
    },
};

struct Setup {
    core: Option<mockcore::Handle>,
    _tmp: tempfile::TempDir,
    db: Arc<Database>,
    proxy: GateProxy,
    events: mpsc::Sender<Event>,
    barriers: mpsc::Sender<BarrierRequest>,
    stager: Option<Stager>,
}

/// A stager over a fresh journal, behind a proxy that has served the hashes
/// of heights `0..=blocks` of a mock bitcoind.
async fn setup(blocks: u64, event_capacity: usize) -> Setup {
    let core = std::thread::spawn(move || {
        let core = mockcore::builder().network(Network::Regtest).build();
        core.mine_blocks(blocks);
        core
    })
    .join()
    .unwrap();
    let (unused_barriers, _) = mpsc::channel(1);
    let proxy = GateProxy::start(
        ProxyConfig {
            upstream_url: core.url(),
            auth: UpstreamAuth::UserPass("user".into(), "pass".into()),
            block_cache: 4,
        },
        unused_barriers,
    )
    .await
    .unwrap();
    proxy.set_chunk_end(blocks.try_into().unwrap());
    let client = reqwest::Client::new();
    for height in 0..=blocks {
        let response = client
            .post(proxy.url())
            .header("content-type", "application/json")
            .body(
                json!({"jsonrpc": "2.0", "id": 1, "method": "getblockhash", "params": [height]})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        let response = response.text().await.unwrap();
        assert!(
            proxy.hash_for(height.try_into().unwrap()).is_some(),
            "{response}"
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(&tmp.path().join("journal.redb"), Chain::Regtest).unwrap();
    let db = journal.db().clone();
    let (events, events_rx) = mpsc::channel(event_capacity);
    let (barriers, barriers_rx) = mpsc::channel(1);
    let stager = Stager::new(journal, events_rx, barriers_rx, proxy.clone());
    Setup {
        core: Some(core),
        _tmp: tmp,
        db,
        proxy,
        events,
        barriers,
        stager: Some(stager),
    }
}

impl Drop for Setup {
    /// mockcore owns a runtime, which can't be dropped inside the test's; its
    /// constructor blocks on one too, so `setup` builds it on a thread.
    fn drop(&mut self) {
        let core = self.core.take();
        std::thread::spawn(move || drop(core)).join().unwrap();
    }
}

impl Setup {
    fn run(&mut self) -> JoinHandle<anyhow::Result<()>> {
        tokio::spawn(self.stager.take().unwrap().run())
    }

    async fn send(&self, events: impl IntoIterator<Item = Event>) {
        for event in events {
            self.events.send(event).await.unwrap();
        }
    }

    async fn barrier(&self) {
        let (tx, rx) = oneshot::channel();
        self.barriers.send(tx).await.unwrap();
        rx.await.unwrap();
    }

    fn hash(&self, height: u32) -> BlockHash {
        self.proxy.hash_for(height).unwrap()
    }
}

fn event(height: u32, tx: u32) -> Event {
    Event::RuneEtched {
        block_height: height,
        rune_id: RuneId {
            block: height.into(),
            tx,
        },
        txid: Txid::all_zeros(),
    }
}

fn raw_rows(db: &Database) -> Vec<(u32, u32, RawEvent)> {
    let tx = db.begin_read().unwrap();
    let table = tx.open_table(table(cf::RAW_EVENTS)).unwrap();
    table
        .iter()
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            let key = key.value();
            (
                u32::from_be_bytes(key[..4].try_into().unwrap()),
                u32::from_be_bytes(key[4..].try_into().unwrap()),
                RawEvent::decode(value.value()).unwrap(),
            )
        })
        .collect()
}

fn raw_heights(db: &Database) -> Vec<(u32, BlockHash)> {
    let tx = db.begin_read().unwrap();
    let table = tx.open_table(table(cf::RAW_HEIGHTS)).unwrap();
    table
        .iter()
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            (
                u32::from_be_bytes(key.value().try_into().unwrap()),
                BlockHash::from_slice(value.value()).unwrap(),
            )
        })
        .collect()
}

fn rows(events: impl IntoIterator<Item = (u32, u32, Event)>) -> Vec<(u32, u32, RawEvent)> {
    events
        .into_iter()
        .map(|(height, seq, event)| (height, seq, RawEvent::from(event)))
        .collect()
}

#[tokio::test]
async fn barrier_syncs_pending_events_before_reply() {
    let mut s = setup(3, 16).await;
    let staged_tip = s.stager.as_ref().unwrap().staged_tip();
    let _run = s.run();

    s.send([event(1, 0), event(1, 1), event(2, 0)]).await;
    s.barrier().await;

    assert_eq!(
        raw_rows(&s.db),
        rows([
            (1, 0, event(1, 0)),
            (1, 1, event(1, 1)),
            (2, 0, event(2, 0))
        ])
    );
    assert_eq!(raw_heights(&s.db), [(1, s.hash(1)), (2, s.hash(2))]);
    assert_eq!(staged_tip.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn replay_replaces_height() {
    let mut s = setup(3, 16).await;
    let _run = s.run();

    s.send([event(3, 0), event(3, 1)]).await;
    s.barrier().await;
    s.send([event(3, 7)]).await;
    s.barrier().await;

    assert_eq!(raw_rows(&s.db), rows([(3, 0, event(3, 7))]));
    assert_eq!(raw_heights(&s.db), [(3, s.hash(3))]);
}

#[tokio::test]
async fn rewind_deletes_higher_heights() {
    let mut s = setup(5, 16).await;
    let staged_tip = s.stager.as_ref().unwrap().staged_tip();
    let _run = s.run();

    s.send([event(1, 0), event(2, 0), event(3, 0)]).await;
    s.barrier().await;
    s.send([event(2, 5)]).await;
    s.barrier().await;

    assert_eq!(
        raw_rows(&s.db),
        rows([(1, 0, event(1, 0)), (2, 0, event(2, 5))])
    );
    assert_eq!(raw_heights(&s.db), [(1, s.hash(1)), (2, s.hash(2))]);
    assert_eq!(staged_tip.load(Ordering::SeqCst), 2);

    s.send([event(4, 0), event(5, 0), event(4, 9)]).await;
    s.barrier().await;

    assert_eq!(
        raw_rows(&s.db),
        rows([
            (1, 0, event(1, 0)),
            (2, 0, event(2, 5)),
            (4, 0, event(4, 9))
        ])
    );
    assert_eq!(
        raw_heights(&s.db),
        [(1, s.hash(1)), (2, s.hash(2)), (4, s.hash(4))]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stager_drains_more_events_than_channel_capacity() {
    let mut s = setup(10, 4).await;
    let _run = s.run();

    // Mirrors ord's updater thread: events, then a barrier it blocks on.
    let sender = s.events.clone();
    let barriers = s.barriers.clone();
    let producer = std::thread::spawn(move || {
        for height in 1..=10 {
            for tx in 0..10 {
                sender.blocking_send(event(height, tx)).unwrap();
            }
        }
        let (tx, rx) = oneshot::channel();
        barriers.blocking_send(tx).unwrap();
        rx.blocking_recv().unwrap();
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || producer.join().unwrap()),
    )
    .await
    .expect("producer deadlocked")
    .unwrap();

    let expected: Vec<_> = (1..=10)
        .flat_map(|height| (0..10).map(move |tx| (height, tx, event(height, tx))))
        .collect();
    assert_eq!(raw_rows(&s.db), rows(expected));
    assert_eq!(raw_heights(&s.db).len(), 10);
}

#[tokio::test]
async fn discard_pending_drops_partial_block_before_retry() {
    let mut s = setup(3, 16).await;
    let handle = s.stager.as_ref().unwrap().handle();
    let _run = s.run();

    s.send([event(2, 0), event(2, 1)]).await;
    handle.discard_pending().await.unwrap();
    assert_eq!(raw_rows(&s.db), []);
    s.send([event(2, 0), event(2, 1), event(2, 2)]).await;
    s.barrier().await;

    assert_eq!(
        raw_rows(&s.db),
        rows([
            (2, 0, event(2, 0)),
            (2, 1, event(2, 1)),
            (2, 2, event(2, 2))
        ])
    );
}

#[tokio::test]
async fn truncate_raw_above_removes_rows_and_heights() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(&tmp.path().join("journal.redb"), Chain::Regtest).unwrap();
    let tx = journal.begin_write().unwrap();
    {
        let mut events = tx.open_table(table(cf::RAW_EVENTS)).unwrap();
        let mut heights = tx.open_table(table(cf::RAW_HEIGHTS)).unwrap();
        for height in 1u32..=3 {
            for seq in 0u32..2 {
                let mut key = height.to_be_bytes().to_vec();
                key.extend(seq.to_be_bytes());
                events
                    .insert(
                        key.as_slice(),
                        RawEvent::from(event(height, seq)).encode().as_slice(),
                    )
                    .unwrap();
            }
            heights
                .insert(
                    height.to_be_bytes().as_slice(),
                    BlockHash::all_zeros().as_byte_array().as_slice(),
                )
                .unwrap();
        }
    }
    tx.commit().unwrap();

    truncate_raw_above(&journal, Some(1)).unwrap();

    assert_eq!(
        raw_rows(journal.db()),
        rows([(1, 0, event(1, 0)), (1, 1, event(1, 1))])
    );
    assert_eq!(raw_heights(journal.db()), [(1, BlockHash::all_zeros())]);

    truncate_raw_above(&journal, None).unwrap();

    assert_eq!(raw_rows(journal.db()), []);
    assert_eq!(raw_heights(journal.db()), []);
}

#[tokio::test]
async fn drained_event_below_committed_count_is_fatal() {
    let mut s = setup(5, 16).await;
    s.stager
        .as_mut()
        .unwrap()
        .set_commit_guard(Arc::new(|| Ok(4)));
    let run = s.run();

    s.send([event(3, 0)]).await;
    let (tx, rx) = oneshot::channel();
    s.barriers.send(tx).await.unwrap();

    assert!(rx.await.is_err());
    let err = run.await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "ord committed ahead of the raw event log: upstream assumption A1 no longer holds"
    );
    assert_eq!(raw_rows(&s.db), []);
}

#[tokio::test]
async fn drain_now_syncs_and_checks_guard() {
    let mut s = setup(5, 16).await;
    let committed = Arc::new(AtomicU32::new(4));
    let stager = s.stager.as_mut().unwrap();
    stager.set_commit_guard({
        let committed = committed.clone();
        Arc::new(move || Ok(committed.load(Ordering::SeqCst)))
    });
    let handle = stager.handle();
    let run = s.run();

    s.send([event(4, 0)]).await;
    handle.drain_now().await.unwrap();
    assert_eq!(raw_rows(&s.db), rows([(4, 0, event(4, 0))]));

    committed.store(6, Ordering::SeqCst);
    s.send([event(5, 0)]).await;
    let err = handle.drain_now().await.unwrap_err();
    assert!(err.to_string().contains("upstream assumption A1"), "{err}");
    assert!(run.await.unwrap().is_err());
    assert_eq!(raw_rows(&s.db), rows([(4, 0, event(4, 0))]));
}

#[tokio::test]
async fn event_without_proxy_hash_is_an_error() {
    let mut s = setup(2, 16).await;
    let run = s.run();

    s.send([event(9, 0)]).await;
    let (tx, rx) = oneshot::channel();
    s.barriers.send(tx).await.unwrap();

    assert!(rx.await.is_err());
    assert!(run.await.unwrap().is_err());
    assert_eq!(raw_rows(&s.db), []);
}

#[tokio::test]
async fn run_ends_when_event_sender_drops() {
    let mut s = setup(2, 16).await;
    let run = s.run();

    s.send([event(1, 0)]).await;
    s.barrier().await;
    s.events = mpsc::channel(1).0;

    run.await.unwrap().unwrap();
    assert_eq!(raw_rows(&s.db), rows([(1, 0, event(1, 0))]));
}
