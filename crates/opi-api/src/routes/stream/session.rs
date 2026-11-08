//! A subscriber's stream: one task per subscriber, which reads the blocks
//! of each module of the stream from that module's database, in order, and
//! queues their frames for the client.
//!
//! Each module has a cursor: the next height to deliver and the blocks the
//! client holds last. The stream delivers the cursor lowest by height, then
//! module name, once the module's block-hash table holds the height plus
//! the confirmations less one; until then it waits, without a database
//! connection, for a poller to report a change, sending a heartbeat every
//! STREAM_HEARTBEAT_SECS. Before each block, and on each change its poller
//! reports, the last block a module's client holds is checked against the
//! stored hash; when it was replaced, the stream sends a rollback to the
//! newest block of the client's that is still stored and goes on after it.
//! A block missing because the module's table is below it (a module behind,
//! or a reorg in progress) is waited for.
//!
//! Each block and rollback event carries the id of every module's position
//! after it, so a resume puts each module back where its client is.
//!
//! The reads take a permit of the module database's stream readers, which
//! leaves the rest of the pool to the other routes; no permit is held while
//! a chunk waits for room in the client's buffer.

use {
    super::{
        Position, StreamStop, format_id,
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
    /// the block its Last-Event-ID named and those the stream delivered, at
    /// most [`WINDOW`].
    pub ring: VecDeque<(i32, String)>,
    /// Whether the cursor is to be checked against the stored hashes.
    pub dirty: bool,
    /// Since when the last block the client holds has waited for its row
    /// to be stored or its reorg to be read; past STREAM_RESUME_WAIT_SECS
    /// it is taken as stale.
    pub waiting_since: Option<Instant>,
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

    /// The module's highest stored height, under a reader permit.
    async fn top(&self) -> Result<Option<i32>, ApiError> {
        let _permit = self.read_permit().await;
        Ok(self.provider.tip(&self.db).await?.map(|(height, _)| height))
    }

    /// The module's lowest stored height, under a reader permit.
    async fn oldest(&self) -> Result<Option<i32>, ApiError> {
        let _permit = self.read_permit().await;
        self.provider.oldest(&self.db).await
    }

    fn push(&mut self, block: (i32, String)) {
        self.ring.push_back(block);
        while self.ring.len() > WINDOW as usize {
            self.ring.pop_front();
        }
    }

    /// The client's position in this module: the height below the next,
    /// with its hash when the client holds that block.
    fn position(&self) -> Option<Position> {
        let height = self.next? - 1;
        let hash = self
            .ring
            .back()
            .filter(|(held, _)| *held == height)
            .map(|(_, hash)| hash.clone());
        Some(Position {
            provider: self.provider,
            height,
            hash,
        })
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

    /// The id of every module's position.
    fn id(&self) -> String {
        let positions: Vec<Position> = self.cursors.iter().filter_map(Cursor::position).collect();
        format_id(&positions)
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
            let cursor = &self.cursors[index];
            if cursor.next.is_none() && cursor.watch.tip.borrow().height.is_some() {
                let oldest = cursor.oldest().await?;
                self.cursors[index].next = oldest;
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

    /// Rolls the cursor back when the last block its client holds is no
    /// longer stored. Answers whether it rolled back. A block above the
    /// module's stored rows is waited for, and so is a rollback the
    /// module's poller has not seen yet: the cursor is checked again on the
    /// next change.
    async fn check(&mut self, index: usize) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        let Some((height, hash)) = cursor.ring.back().cloned() else {
            return Ok(false);
        };
        let kept = match cursor.stored(height).await? {
            Some(stored) if stored == hash => {
                self.cursors[index].waiting_since = None;
                return Ok(false);
            }
            Some(_) => self.kept(index).await?,
            None if cursor.top().await?.is_none_or(|top| top < height) => None,
            None => self.kept(index).await?,
        };
        let kept = match kept {
            Some(kept) => kept,
            None => {
                let cursor = &mut self.cursors[index];
                let since = *cursor.waiting_since.get_or_insert_with(Instant::now);
                if since.elapsed() < self.settings.resume_wait {
                    // checked again on every wake, heartbeats included
                    cursor.dirty = true;
                    return Ok(false);
                }
                let (front, _) = cursor
                    .ring
                    .front()
                    .cloned()
                    .expect("a block the client holds");
                tracing::info!(
                    "stream: {} block {height} was not stored or its reorg read within {:?}; rolling back as deep as a reorg goes",
                    cursor.provider.name(),
                    self.settings.resume_wait,
                );
                self.deepest(index, front).await?
            }
        };
        self.cursors[index].waiting_since = None;
        self.rollback(index, kept).await?;
        Ok(true)
    }

    /// Delivers block `height` of the cursor at `index` when it is due.
    /// Answers whether it delivered or rolled back.
    async fn deliver(&mut self, index: usize, height: i32) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        let (provider, db) = (cursor.provider, cursor.db.clone());
        let due_at = height + self.confirmations - 1;
        let rows = cursor.rows(&[height - 1, height, due_at]).await?;
        if let Some((last, hash)) = cursor.ring.back()
            && !rows
                .iter()
                .any(|row| row.height == *last && &row.hash == hash)
        {
            return self.check(index).await;
        }
        let Some(row) = rows.iter().find(|row| row.height == height).cloned() else {
            // a reorg in progress, or a block pruning has deleted
            if let Some(oldest) = cursor.oldest().await?
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
        let cursor = &mut self.cursors[index];
        cursor.push((height, row.hash));
        cursor.next = Some(height + 1);
        let id = self.id();
        self.send(format!("]}}\nid: {id}\n\n")).await?;
        global().stream_message("block");
        Ok(true)
    }

    /// Sends the rollback of the cursor at `index` to block `kept`, the last
    /// block of the client's branch still stored, and goes on after it.
    async fn rollback(&mut self, index: usize, kept: i32) -> Result<(), End> {
        let kept_hash = self.cursors[index].stored(kept).await?;
        let cursor = &mut self.cursors[index];
        let name = cursor.provider.name();
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
        let data = format!("{{\"module\":\"{name}\",\"height\":{kept}}}");
        let id = self.id();
        self.send(frame("rollback", &data, Some(&id))).await?;
        global().stream_message("rollback");
        Ok(())
    }

    /// The last block the client holds of the cursor's that is still
    /// stored: the newest block of its ring with the stored hash, read in
    /// one statement. A reorg below the whole ring, such as one below a
    /// Last-Event-ID's block, is followed back from the ring's first block
    /// through the reorgs the module's poller saw, and past those the
    /// rollback goes as deep as the indexer's. `None` while the poller
    /// still holds the ring's first block, so has not read the reorg yet.
    async fn kept(&self, index: usize) -> Result<Option<i32>, End> {
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
            return Ok(Some(*height));
        }
        let (front, hash) = cursor
            .ring
            .front()
            .cloned()
            .expect("a block the client holds");
        if let Some((kept, _)) = self.follow(index, front, hash.clone()).await? {
            return Ok(Some(kept.min(front - 1)));
        }
        let unseen = cursor
            .watch
            .history
            .lock()
            .expect("the history lock")
            .holds(front, &hash);
        if unseen {
            return Ok(None);
        }
        Ok(Some(self.deepest(index, front).await?))
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
        let oldest = self.cursors[index].oldest().await?;
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
