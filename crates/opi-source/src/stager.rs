//! Stager: syncs ord's events to the journal's raw event log at each commit
//! barrier, so every event of every block ord commits is on disk first.

use {
    crate::{
        journal::{Journal, begin_write, cf, raw_event_key, table},
        proxy::{BarrierRequest, GateProxy},
        raw_event::RawEvent,
    },
    anyhow::{Result, anyhow, bail},
    bitcoin::{BlockHash, hashes::Hash},
    ord::index::event::Event,
    redb::{ReadableDatabase, ReadableTable, Table},
    std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
    },
    tokio::sync::{mpsc, oneshot},
};

/// Returns ord's committed block count. It runs on a blocking thread while
/// ord's updater may be held at a barrier, so it must not call bitcoind
/// through the proxy: that call would wait on its own barrier and deadlock.
pub type CommitGuard = Arc<dyn Fn() -> Result<u32> + Send + Sync>;

type Reply = oneshot::Sender<Result<()>>;

/// A request from a [`StagerHandle`].
enum Request {
    Drain(Reply),
    Discard(Reply),
}

/// Buffers ord's events by height and writes them to `raw_events` and
/// `raw_heights` at each barrier, in one durable write transaction.
///
/// When ord's `update()` fails, the events of the block it was indexing may
/// be pending without that block's barrier. The driver calls
/// [`StagerHandle::discard_pending`] after a failed `update()` and before
/// retrying it, so the retry's events replace that partial block.
pub struct Stager {
    journal: Journal,
    events: mpsc::Receiver<Event>,
    barriers: mpsc::Receiver<BarrierRequest>,
    requests: mpsc::Receiver<Request>,
    request_sender: mpsc::Sender<Request>,
    proxy: GateProxy,
    guard: Option<CommitGuard>,
    staged_tip: Arc<AtomicU32>,
}

/// Cloneable handle for driver requests outside a proxy barrier.
#[derive(Clone)]
pub struct StagerHandle {
    requests: mpsc::Sender<Request>,
}

impl StagerHandle {
    /// Drains the event channel and syncs everything pending, with the same
    /// commit guard check as a barrier. Fails when the stager has stopped or
    /// the sync fails; a failed sync also stops the stager.
    ///
    /// Call it only while ord's updater is idle (before the first `update()`
    /// or after one returns). A sync replaces each pending height, so a sync
    /// in the middle of a block would store part of it, and the barrier that
    /// follows would replace that part with the block's remaining events.
    pub async fn drain_now(&self) -> Result<()> {
        self.request(Request::Drain).await
    }

    /// Drains the event channel and drops every pending event, keeping any
    /// pending rewind. It writes nothing. The driver calls it after a failed
    /// `update()` and before retrying it.
    pub async fn discard_pending(&self) -> Result<()> {
        self.request(Request::Discard).await
    }

    async fn request(&self, request: fn(Reply) -> Request) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(request(tx))
            .await
            .map_err(|_| anyhow!("stager stopped"))?;
        rx.await.map_err(|_| anyhow!("stager stopped"))?
    }
}

impl Stager {
    pub fn new(
        journal: Journal,
        events: mpsc::Receiver<Event>,
        barriers: mpsc::Receiver<BarrierRequest>,
        proxy: GateProxy,
    ) -> Stager {
        let (request_sender, requests) = mpsc::channel(1);
        Stager {
            journal,
            events,
            barriers,
            requests,
            request_sender,
            proxy,
            guard: None,
            staged_tip: Arc::default(),
        }
    }

    /// Sets the check run at every sync: the guard is called before draining,
    /// and a drained event below the count it returns is fatal.
    pub fn set_commit_guard(&mut self, guard: CommitGuard) {
        self.guard = Some(guard);
    }

    pub fn handle(&self) -> StagerHandle {
        StagerHandle {
            requests: self.request_sender.clone(),
        }
    }

    /// Highest height whose events are synced. It reads 0 until `run` loads
    /// the journal's raw log.
    pub fn staged_tip(&self) -> Arc<AtomicU32> {
        self.staged_tip.clone()
    }

    /// Runs until ord drops its event sender, or a sync fails. Events still
    /// pending at that point belong to blocks ord never committed. A failed
    /// sync drops its barrier unanswered and fails a pending `drain_now`.
    pub async fn run(self) -> Result<()> {
        let Stager {
            journal,
            events,
            barriers,
            requests,
            request_sender: _,
            proxy,
            guard,
            staged_tip,
        } = self;
        let mut runner = Runner {
            staged: last_raw_height(&journal)?,
            journal,
            proxy,
            guard,
            staged_tip,
            pending: BTreeMap::new(),
            rewind_to: None,
        };
        runner.publish();
        runner.run(events, barriers, requests).await
    }
}

struct Runner {
    journal: Journal,
    proxy: GateProxy,
    guard: Option<CommitGuard>,
    staged_tip: Arc<AtomicU32>,
    pending: BTreeMap<u32, Vec<RawEvent>>,
    /// Lowest height a replay restarted from since the last sync. The sync
    /// deletes every raw height above it in the same transaction.
    rewind_to: Option<u32>,
    /// Highest height with synced events, lowered to a rewind height until
    /// the next sync.
    staged: Option<u32>,
}

impl Runner {
    async fn run(
        &mut self,
        mut events: mpsc::Receiver<Event>,
        mut barriers: mpsc::Receiver<BarrierRequest>,
        mut requests: mpsc::Receiver<Request>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => self.buffer(event),
                    None => return Ok(()),
                },
                Some(reply) = barriers.recv() => {
                    self.sync(&mut events).await?;
                    reply.send(()).ok();
                }
                Some(request) = requests.recv() => match request {
                    Request::Drain(reply) => {
                        if let Err(err) = self.sync(&mut events).await {
                            reply.send(Err(anyhow!("{err:#}"))).ok();
                            return Err(err);
                        }
                        reply.send(Ok(())).ok();
                    }
                    Request::Discard(reply) => {
                        self.discard(&mut events);
                        reply.send(Ok(())).ok();
                    }
                },
            }
        }
    }

    /// Drains the event channel and drops every pending event. `rewind_to`
    /// stays, so the next sync still deletes rows above a replay's start.
    fn discard(&mut self, events: &mut mpsc::Receiver<Event>) {
        while let Ok(event) = events.try_recv() {
            self.buffer(event);
        }
        self.pending.clear();
    }

    /// Adds `event` to its height. An event below the highest pending or
    /// staged height starts a replay: pending heights from it up are
    /// dropped, and the next sync deletes raw rows above it.
    fn buffer(&mut self, event: Event) {
        let event = RawEvent::from(event);
        let height = event.height();
        let top = self.pending.keys().next_back().copied().max(self.staged);
        if top.is_some_and(|top| height < top) {
            self.pending.split_off(&height);
            self.rewind_to = Some(self.rewind_to.map_or(height, |r| r.min(height)));
            self.staged = self.staged.map(|staged| staged.min(height));
        }
        self.pending.entry(height).or_default().push(event);
    }

    /// Drains the event channel and writes every pending height, tagged with
    /// its block hash, in one durable transaction.
    async fn sync(&mut self, events: &mut mpsc::Receiver<Event>) -> Result<()> {
        let committed = match self.guard.clone() {
            Some(guard) => Some(tokio::task::spawn_blocking(move || guard()).await??),
            None => None,
        };
        while let Ok(event) = events.try_recv() {
            self.buffer(event);
        }
        if let (Some(count), Some(&lowest)) = (committed, self.pending.keys().next())
            && lowest < count
        {
            bail!(
                "ord committed ahead of the raw event log: upstream assumption A1 no longer holds"
            );
        }
        if self.pending.is_empty() && self.rewind_to.is_none() {
            return Ok(());
        }

        let mut heights = Vec::with_capacity(self.pending.len());
        for (height, events) in std::mem::take(&mut self.pending) {
            let hash = self.proxy.hash_for(height).ok_or_else(|| {
                anyhow!("ord sent events for height {height}, whose hash the proxy never served")
            })?;
            heights.push((height, hash, events));
        }

        let journal = self.journal.clone();
        let rewind_to = self.rewind_to.take();
        let top = tokio::task::spawn_blocking(move || write_heights(&journal, rewind_to, &heights))
            .await??;

        self.staged = top;
        self.publish();
        Ok(())
    }

    fn publish(&self) {
        self.staged_tip
            .store(self.staged.unwrap_or(0), Ordering::SeqCst);
    }
}

type RawTable<'txn> = Table<'txn, &'static [u8], &'static [u8]>;

/// Deletes raw rows above `rewind_to`, then writes `heights`, one
/// transaction per segment from the lowest up. Returns the highest height
/// left in the raw log.
///
/// A crash between two segments' commits leaves the lower heights written
/// and the higher ones not, as if the sync had stopped there: ord commits no
/// height before the sync returns, and a restart drops raw rows above ord's
/// committed height. Rows above `rewind_to` that a crash leaves behind carry
/// the hash of a block ord no longer indexes, so finalize ignores them until
/// they are replaced.
fn write_heights(
    journal: &Journal,
    rewind_to: Option<u32>,
    heights: &[(u32, BlockHash, Vec<RawEvent>)],
) -> Result<Option<u32>> {
    let mut starts = heights
        .iter()
        .map(|(height, ..)| journal.segment_start(*height))
        .collect::<BTreeSet<_>>();
    if let Some(rewind_to) = rewind_to {
        starts.extend(
            journal
                .segment_starts()
                .into_iter()
                .filter(|start| start + (journal.segment_len() - 1) > rewind_to),
        );
    }
    for start in starts {
        let segment = journal.segment_mut(start)?;
        let tx = begin_write(&segment)?;
        {
            let mut events = tx.open_table(table(cf::RAW_EVENTS))?;
            let mut hashes = tx.open_table(table(cf::RAW_HEIGHTS))?;
            if let Some(height) = rewind_to {
                delete_above(&mut events, &mut hashes, Some(height))?;
            }
            for (height, hash, list) in heights
                .iter()
                .filter(|(height, ..)| journal.segment_start(*height) == start)
            {
                let first = raw_event_key(*height, 0);
                let last = raw_event_key(*height, u32::MAX);
                events.retain_in::<&[u8], _>(first.as_slice()..=last.as_slice(), |_, _| false)?;
                for (seq, event) in list.iter().enumerate() {
                    let key = raw_event_key(*height, u32::try_from(seq)?);
                    events.insert(key.as_slice(), event.encode().as_slice())?;
                }
                hashes.insert(
                    height.to_be_bytes().as_slice(),
                    hash.as_byte_array().as_slice(),
                )?;
            }
        }
        tx.commit()?;
    }
    last_raw_height(journal)
}

/// Deletes `raw_events` and `raw_heights` rows above `height`, or all rows
/// when `height` is `None`.
fn delete_above(events: &mut RawTable, hashes: &mut RawTable, height: Option<u32>) -> Result<()> {
    let from = match height {
        None => 0,
        Some(u32::MAX) => return Ok(()),
        Some(height) => height + 1,
    };
    let first = raw_event_key(from, 0);
    events.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    let first = from.to_be_bytes();
    hashes.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
    Ok(())
}

fn last_height(hashes: &impl ReadableTable<&'static [u8], &'static [u8]>) -> Result<Option<u32>> {
    let Some((key, _)) = hashes.last()? else {
        return Ok(None);
    };
    let key: [u8; 4] = key
        .value()
        .try_into()
        .map_err(|_| anyhow!("raw_heights key is not 4 bytes"))?;
    Ok(Some(u32::from_be_bytes(key)))
}

/// Highest height in the raw log of any segment.
fn last_raw_height(journal: &Journal) -> Result<Option<u32>> {
    for start in journal.segment_starts().into_iter().rev() {
        if let Some(segment) = journal.segment(start)?
            && let Some(height) =
                last_height(&segment.begin_read()?.open_table(table(cf::RAW_HEIGHTS))?)?
        {
            return Ok(Some(height));
        }
    }
    Ok(None)
}

/// Deletes raw log rows above `height` (all rows when `None`), one durable
/// transaction per segment that holds any. Run at startup with ord's
/// committed height, it drops events of blocks ord indexed but never
/// committed.
pub fn truncate_raw_above(journal: &Journal, height: Option<u32>) -> Result<()> {
    for start in journal.segment_starts() {
        if height.is_some_and(|height| start + (journal.segment_len() - 1) <= height) {
            continue;
        }
        let Some(segment) = journal.segment(start)? else {
            continue;
        };
        let tx = begin_write(&segment)?;
        {
            let mut events = tx.open_table(table(cf::RAW_EVENTS))?;
            let mut hashes = tx.open_table(table(cf::RAW_HEIGHTS))?;
            delete_above(&mut events, &mut hashes, height)?;
        }
        tx.commit()?;
    }
    Ok(())
}
