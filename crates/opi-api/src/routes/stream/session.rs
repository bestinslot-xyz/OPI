//! A subscriber's stream: one task per subscriber, which reads the blocks
//! of each module of the stream from that module's database, in order, and
//! queues their frames for the client.
//!
//! Each module has a cursor: the next height to deliver and the blocks
//! delivered last. The stream delivers the cursor lowest by height, then
//! module name, once the module's block-hash table holds the height plus
//! the confirmations less one; until then it waits, without a database
//! connection, for a poller to report a change, sending a heartbeat every
//! STREAM_HEARTBEAT_SECS. Before each block, and on each change its poller
//! reports, a module's last delivered block is checked against the stored
//! hash; when it was replaced, the stream sends a rollback to the last
//! delivered block still stored and goes on after it.
//!
//! A block's reads take a permit of its module database's stream readers,
//! which leaves the rest of the pool to the other routes; no permit is held
//! while a chunk waits for room in the client's buffer.

use {
    super::{
        StreamStop,
        poller::{WINDOW, Watch},
        provider::{BlockRow, Provider},
    },
    crate::{
        config::StreamSettings,
        db::Db,
        js::{JsValue, json_stringify},
        reply::ApiError,
    },
    axum::body::Bytes,
    opi_core::metrics::{StreamSubscriberGuard, global},
    std::{
        collections::{HashMap, VecDeque},
        sync::Arc,
        time::Duration,
    },
    tokio::{
        sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit, mpsc, oneshot},
        time::Instant,
    },
};

/// The events a block message's chunks carry at most.
pub const EVENT_PAGE: i64 = 100;

/// The most reorgs a rollback follows back through the pollers' records.
const MAX_FORKS: usize = 64;

/// A module of the stream.
pub struct Cursor {
    pub provider: Provider,
    pub db: Db,
    pub watch: Watch,
    /// The permits of the module database's stream readers.
    pub reads: Arc<Semaphore>,
    /// The next height to deliver; `None` until the module's first block is
    /// indexed, for a stream that named no start.
    pub next: Option<i32>,
    /// The last blocks the client holds, as height and hash, oldest first:
    /// those the stream delivered, at most [`WINDOW`], after the block a
    /// Last-Event-ID named.
    pub ring: VecDeque<(i32, String)>,
    /// A block of the stream's chain the client holds, from a
    /// Last-Event-ID naming an earlier module at the same height: checked
    /// once the module's tip reaches it.
    pub pending: Option<(i32, String)>,
    /// Whether the cursor is to be checked against the stored hashes.
    pub dirty: bool,
}

/// Why a stream ends.
enum End {
    /// The client has gone.
    Closed,
    /// The server is shutting down.
    Stopped,
    /// The client could not take a message for a heartbeat interval.
    Slow,
    /// A query failed.
    Failed(ApiError),
    /// The stream cannot go on, for the reason given.
    Gone(String),
}

/// What ended a stream's wait.
#[derive(PartialEq, Eq)]
enum Wake {
    Changed,
    Heartbeat,
}

impl From<ApiError> for End {
    fn from(err: ApiError) -> End {
        End::Failed(err)
    }
}

/// A subscriber's stream.
pub struct Session {
    pub settings: StreamSettings,
    pub confirmations: i32,
    /// By module name.
    pub cursors: Vec<Cursor>,
    pub chunks: mpsc::Sender<Bytes>,
    pub end: oneshot::Sender<Option<Bytes>>,
    pub stop: StreamStop,
    pub permit: OwnedSemaphorePermit,
    pub gauge: StreamSubscriberGuard,
}

/// A frame of `event` with `data`, and its id last, so an id comes only
/// with a whole frame.
fn frame(event: &str, data: &str, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("event: {event}\ndata: {data}\nid: {id}\n\n"),
        None => format!("event: {event}\ndata: {data}\n\n"),
    }
}

fn js_string(text: &str) -> String {
    json_stringify(&JsValue::from(text)).unwrap_or_default()
}

impl Cursor {
    /// Waits for a permit of the module database's stream readers.
    async fn read_permit(&self) -> SemaphorePermit<'_> {
        self.reads
            .acquire()
            .await
            .expect("the readers' semaphore stays open")
    }

    /// The rows of `heights`, under a reader permit.
    async fn rows(&self, heights: &[i32]) -> Result<Vec<BlockRow>, ApiError> {
        let _permit = self.read_permit().await;
        self.provider.blocks_at(&self.db, heights).await
    }

    /// The stored hash at `height`.
    async fn stored(&self, height: i32) -> Result<Option<String>, ApiError> {
        Ok(self
            .rows(&[height])
            .await?
            .into_iter()
            .next()
            .map(|row| row.hash))
    }

    fn push(&mut self, block: (i32, String)) {
        self.ring.push_back(block);
        while self.ring.len() > WINDOW as usize {
            self.ring.pop_front();
        }
    }
}

impl Session {
    /// Runs the stream until it ends.
    pub async fn run(mut self) {
        let end = self.serve().await;
        let last = match end {
            End::Closed | End::Stopped => None,
            End::Slow => Some(
                "the client fell behind the stream; reconnect with Last-Event-ID to resume"
                    .to_string(),
            ),
            End::Failed(err) => {
                tracing::error!("stream: {}", err.log_text());
                Some("internal error".to_string())
            }
            End::Gone(reason) => Some(reason),
        };
        let last = last.map(|reason| {
            let data = format!("{{\"error\":{}}}", js_string(&reason));
            Bytes::from(frame("error", &data, None))
        });
        self.end.send(last).ok();
        drop(self.permit);
        drop(self.gauge);
    }

    async fn serve(&mut self) -> End {
        let mut next_heartbeat = Instant::now() + self.settings.heartbeat;
        loop {
            if self.stop.is_stopped() {
                return End::Stopped;
            }
            for cursor in &mut self.cursors {
                if cursor.watch.tip.has_changed().unwrap_or(false) {
                    cursor.watch.tip.borrow_and_update();
                    cursor.dirty = true;
                }
            }
            match self.step().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(end) => return end,
            }
            let mut stop = self.stop.clone();
            let changed = futures::future::select_all(self.cursors.iter_mut().map(|cursor| {
                Box::pin(async {
                    if cursor.watch.tip.changed().await.is_err() {
                        // the poller has stopped, at shutdown
                        std::future::pending::<()>().await;
                    }
                    cursor.dirty = true;
                })
            }));
            let woken = tokio::select! {
                _ = changed => Wake::Changed,
                () = tokio::time::sleep_until(next_heartbeat) => Wake::Heartbeat,
                () = stop.stopped() => return End::Stopped,
                () = self.chunks.closed() => return End::Closed,
            };
            if woken == Wake::Heartbeat {
                if let Err(end) = self.heartbeat().await {
                    return end;
                }
                next_heartbeat = Instant::now() + self.settings.heartbeat;
            }
        }
    }

    /// Checks the dirty cursors and delivers the next block when it is due.
    /// Answers whether it delivered or rolled back anything.
    async fn step(&mut self) -> Result<bool, End> {
        for index in 0..self.cursors.len() {
            if self.cursors[index].dirty {
                self.cursors[index].dirty = false;
                if self.check(index).await? {
                    return Ok(true);
                }
            }
        }
        for index in 0..self.cursors.len() {
            let cursor = &mut self.cursors[index];
            if cursor.next.is_none() && cursor.watch.tip.borrow().height.is_some() {
                cursor.next = cursor.provider.oldest(&cursor.db).await?;
            }
        }
        let Some(index) = (0..self.cursors.len())
            .filter(|&i| self.cursors[i].next.is_some())
            .min_by_key(|&i| (self.cursors[i].next, self.cursors[i].provider))
        else {
            return Ok(false);
        };
        let cursor = &self.cursors[index];
        let next = cursor.next.expect("a cursor with a next height");
        // the poller's tip spares the reads of a block that is not due yet;
        // the block's own read decides
        let due = cursor.watch.tip.borrow().height.is_some_and(|tip| {
            i64::from(tip) >= i64::from(next) + i64::from(self.confirmations) - 1
        });
        if !due {
            return Ok(false);
        }
        self.deliver(index, next).await
    }

    /// Rolls the cursor back when the last block the client holds is no
    /// longer stored. Answers whether it rolled back.
    async fn check(&mut self, index: usize) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        if let Some((height, hash)) = cursor.pending.clone() {
            let reached = cursor
                .watch
                .tip
                .borrow()
                .height
                .is_some_and(|tip| tip >= height);
            if !reached {
                return Ok(false);
            }
            if cursor.stored(height).await?.as_ref() == Some(&hash) {
                self.cursors[index].pending = None;
                return Ok(false);
            }
            let kept = self.follow(index, height, hash).await?;
            let kept = match kept {
                Some((kept, _)) => kept,
                None => self.deepest(index, height).await?,
            };
            self.rollback(index, kept.min(height - 1)).await?;
            return Ok(true);
        }
        let Some((height, hash)) = cursor.ring.back() else {
            return Ok(false);
        };
        if cursor.stored(*height).await?.as_ref() == Some(hash) {
            return Ok(false);
        }
        let kept = self.kept(index).await?;
        self.rollback(index, kept).await?;
        Ok(true)
    }

    /// Delivers block `height` of the cursor at `index` when it is due.
    /// Answers whether it delivered or rolled back.
    async fn deliver(&mut self, index: usize, height: i32) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        if cursor.pending.is_some() {
            // its check comes first, once the tip reaches it
            return Ok(false);
        }
        let (provider, db) = (cursor.provider, cursor.db.clone());
        let due_at = height + self.confirmations - 1;
        let rows = cursor.rows(&[height - 1, height, due_at]).await?;
        if let Some((last, hash)) = cursor.ring.back()
            && !rows
                .iter()
                .any(|row| row.height == *last && &row.hash == hash)
        {
            let kept = self.kept(index).await?;
            self.rollback(index, kept).await?;
            return Ok(true);
        }
        let Some(row) = rows.iter().find(|row| row.height == height).cloned() else {
            // a reorg in progress, or a block pruning has deleted
            if let Some(oldest) = provider.oldest(&db).await?
                && oldest > height
            {
                return Err(End::Gone(format!(
                    "block {height} of {} is no longer kept; the oldest is {oldest}",
                    provider.name()
                )));
            }
            return Ok(false);
        };
        if !rows.iter().any(|row| row.height == due_at) {
            return Ok(false);
        }
        let range = {
            let _permit = cursor.read_permit().await;
            provider.event_ids(&db, height).await?
        };
        let name = provider.name();
        let time = row.time.map_or("null".to_string(), |time| time.to_string());
        let head = format!(
            "event: block\ndata: {{\"module\":\"{name}\",\"height\":{height},\"hash\":{},\"block_time\":{time}{},\"events\":[",
            js_string(&row.hash),
            provider.header_fields(),
        );
        self.send(head).await?;
        if let Some((low, high)) = range {
            let mut after = low - 1;
            let mut first = true;
            loop {
                let page = {
                    let cursor = &self.cursors[index];
                    let _permit = cursor.read_permit().await;
                    provider
                        .events(&db, height, after, high, EVENT_PAGE)
                        .await?
                };
                let Some((last_id, _)) = page.last() else {
                    break;
                };
                after = *last_id;
                let full = page.len() as i64 == EVENT_PAGE;
                let mut chunk = String::new();
                for (_, event) in &page {
                    if !first {
                        chunk.push(',');
                    }
                    first = false;
                    chunk.push_str(&json_stringify(event).unwrap_or_default());
                }
                self.send(chunk).await?;
                if !full {
                    break;
                }
            }
        }
        self.send(format!("]}}\nid: {name}:{height}:{}\n\n", row.hash))
            .await?;
        global().stream_message("block");
        let cursor = &mut self.cursors[index];
        cursor.push((height, row.hash));
        cursor.next = Some(height + 1);
        Ok(true)
    }

    /// Sends the rollback of the cursor at `index` to block `kept`, the last
    /// block of the client's branch still stored, and goes on after it.
    async fn rollback(&mut self, index: usize, kept: i32) -> Result<(), End> {
        let cursor = &self.cursors[index];
        let name = cursor.provider.name();
        let kept_hash = cursor.stored(kept).await?;
        let data = format!("{{\"module\":\"{name}\",\"height\":{kept}}}");
        let id = kept_hash
            .as_ref()
            .map(|hash| format!("{name}:{kept}:{hash}"));
        self.send(frame("rollback", &data, id.as_deref())).await?;
        global().stream_message("rollback");
        let cursor = &mut self.cursors[index];
        cursor.pending = None;
        cursor.next = Some(kept + 1);
        let in_ring = cursor
            .ring
            .iter()
            .any(|(height, hash)| *height == kept && Some(hash) == kept_hash.as_ref());
        if in_ring {
            while cursor.ring.back().is_some_and(|(height, _)| *height > kept) {
                cursor.ring.pop_back();
            }
        } else {
            cursor.ring.clear();
            if let Some(hash) = kept_hash {
                cursor.ring.push_back((kept, hash));
            }
        }
        Ok(())
    }

    /// The last block the client holds of the cursor's that is still
    /// stored: the newest block of its ring with the stored hash, read in
    /// one statement. A reorg below the whole ring, such as one below a
    /// Last-Event-ID's block, is followed back from the ring's first block
    /// through the reorgs the module's poller saw, and past those the
    /// rollback goes as deep as the indexer's.
    async fn kept(&self, index: usize) -> Result<i32, End> {
        let cursor = &self.cursors[index];
        let (low, _) = cursor
            .ring
            .front()
            .cloned()
            .expect("a block the client holds");
        let (high, _) = cursor
            .ring
            .back()
            .cloned()
            .expect("a block the client holds");
        let stored: HashMap<i32, String> = {
            let _permit = cursor.read_permit().await;
            cursor.provider.blocks(&cursor.db, low, high).await?
        }
        .into_iter()
        .map(|row| (row.height, row.hash))
        .collect();
        if let Some((height, _)) = cursor
            .ring
            .iter()
            .rev()
            .find(|(height, hash)| stored.get(height) == Some(hash))
        {
            return Ok(*height);
        }
        let (front, hash) = cursor
            .ring
            .front()
            .cloned()
            .expect("a block the client holds");
        match self.follow(index, front, hash).await? {
            Some((kept, _)) => Ok(kept.min(front - 1)),
            None => self.deepest(index, front).await,
        }
    }

    /// The block of `hash` at `height`, followed back through the reorgs
    /// the module's poller saw to one still stored, or to the block a reorg
    /// kept below the poller's window; `None` past what the poller saw.
    async fn follow(
        &self,
        index: usize,
        mut height: i32,
        mut hash: String,
    ) -> Result<Option<(i32, Option<String>)>, End> {
        let cursor = &self.cursors[index];
        for _ in 0..MAX_FORKS {
            if cursor.stored(height).await?.as_ref() == Some(&hash) {
                return Ok(Some((height, Some(hash))));
            }
            let kept = cursor
                .watch
                .history
                .lock()
                .expect("the history lock")
                .kept(height, &hash);
            match kept {
                Some((kept, Some(kept_hash))) => (height, hash) = (kept, kept_hash),
                Some((kept, None)) => return Ok(Some((kept, None))),
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The deepest rollback below `height`: [`WINDOW`] blocks, the deepest
    /// the indexer walks, and not below the module's first stored block.
    async fn deepest(&self, index: usize, height: i32) -> Result<i32, End> {
        Ok((height - WINDOW)
            .max(self.floor(index).await?)
            .min(height - 1))
    }

    /// The height below the module's first stored block, 0 for an empty
    /// table.
    async fn floor(&self, index: usize) -> Result<i32, End> {
        let cursor = &self.cursors[index];
        let oldest = {
            let _permit = cursor.read_permit().await;
            cursor.provider.oldest(&cursor.db).await?
        };
        Ok(oldest.map_or(0, |oldest| (oldest - 1).max(0)))
    }

    async fn heartbeat(&mut self) -> Result<(), End> {
        let tips: Vec<String> = self
            .cursors
            .iter()
            .map(|cursor| {
                let tip = cursor.watch.tip.borrow().height;
                let tip = tip.map_or("null".to_string(), |tip| tip.to_string());
                format!("\"{}\":{tip}", cursor.provider.name())
            })
            .collect();
        let data = format!("{{\"tips\":{{{}}}}}", tips.join(","));
        self.send(frame("heartbeat", &data, None)).await?;
        global().stream_message("heartbeat");
        Ok(())
    }

    /// Queues `chunk`, waiting at most a heartbeat interval for room.
    async fn send(&mut self, chunk: String) -> Result<(), End> {
        let mut stop = self.stop.clone();
        let wait: Duration = self.settings.heartbeat;
        tokio::select! {
            sent = tokio::time::timeout(wait, self.chunks.send(Bytes::from(chunk))) => match sent {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(End::Closed),
                Err(_) => Err(End::Slow),
            },
            () = stop.stopped() => Err(End::Stopped),
        }
    }
}
