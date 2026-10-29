//! A subscriber's stream: one task per subscriber, which reads the blocks
//! of each module of the stream from that module's database, in order, and
//! queues their frames for the client.
//!
//! Each module has a cursor: the next height to deliver and the last block
//! delivered. The stream delivers the cursor lowest by height, then module
//! name, once its module's indexed tip has reached the height plus the
//! confirmations less one; until then it waits, without a database
//! connection, for a poller to report a change, sending a heartbeat every
//! STREAM_HEARTBEAT_SECS. Before each block, and on each change its poller
//! reports, a module's last delivered block is checked against the stored
//! hash; when it was replaced, the stream sends a rollback to the last block
//! still stored and goes on after it.

use {
    super::{
        StreamStop,
        poller::{WINDOW, Watch},
        provider::Provider,
    },
    crate::{
        config::StreamSettings,
        db::Db,
        js::{JsValue, json_stringify},
        reply::ApiError,
    },
    axum::body::Bytes,
    opi_core::metrics::{StreamSubscriberGuard, global},
    std::time::Duration,
    tokio::{
        sync::{OwnedSemaphorePermit, mpsc, oneshot},
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
    /// The next height to deliver; `None` until the module's first block is
    /// indexed, for a stream that named no start.
    pub next: Option<i32>,
    /// The last block delivered (or, on a resume, received), as height and
    /// hash.
    pub last: Option<(i32, String)>,
    /// Whether `last` is to be checked against the stored hash.
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

/// A frame of `event` with `data`, and the id of a block.
fn frame(event: &str, id: Option<&str>, data: &str) -> String {
    match id {
        Some(id) => format!("event: {event}\nid: {id}\ndata: {data}\n\n"),
        None => format!("event: {event}\ndata: {data}\n\n"),
    }
}

fn js_string(text: &str) -> String {
    json_stringify(&JsValue::from(text)).unwrap_or_default()
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
            global().stream_message("error");
            let data = format!("{{\"error\":{}}}", js_string(&reason));
            Bytes::from(frame("error", None, &data))
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
        let due = cursor.watch.tip.borrow().height.is_some_and(|tip| {
            i64::from(tip) >= i64::from(next) + i64::from(self.confirmations) - 1
        });
        if !due {
            return Ok(false);
        }
        self.deliver(index, next).await
    }

    /// Rolls the cursor back when its last delivered block is no longer
    /// stored. Answers whether it rolled back.
    async fn check(&mut self, index: usize) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        let Some((height, hash)) = &cursor.last else {
            return Ok(false);
        };
        let stored = cursor.provider.blocks(&cursor.db, *height, *height).await?;
        if stored.first().is_some_and(|row| &row.hash == hash) {
            return Ok(false);
        }
        self.rollback(index).await?;
        Ok(true)
    }

    /// Delivers block `height` of the cursor at `index`. Answers whether
    /// it delivered or rolled back.
    async fn deliver(&mut self, index: usize, height: i32) -> Result<bool, End> {
        let cursor = &self.cursors[index];
        let (provider, db) = (cursor.provider, cursor.db.clone());
        let rows = provider.blocks(&db, height - 1, height).await?;
        if let Some((last, hash)) = &cursor.last
            && !rows
                .iter()
                .any(|row| row.height == *last && &row.hash == hash)
        {
            self.rollback(index).await?;
            return Ok(true);
        }
        let Some(row) = rows.into_iter().find(|row| row.height == height) else {
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
        let name = provider.name();
        let id = format!("{name}:{height}:{}", row.hash);
        let time = row.time.map_or("null".to_string(), |time| time.to_string());
        let head = format!(
            "{{\"module\":\"{name}\",\"height\":{height},\"hash\":{},\"block_time\":{time}{},\"events\":[",
            js_string(&row.hash),
            provider.header_fields(),
        );
        let head = format!("event: block\nid: {id}\ndata: {head}");
        self.send(head).await?;
        if let Some((low, high)) = provider.event_ids(&db, height).await? {
            let mut after = low - 1;
            let mut first = true;
            loop {
                let page = provider
                    .events(&db, height, after, high, EVENT_PAGE)
                    .await?;
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
        self.send("]}\n\n".to_string()).await?;
        global().stream_message("block");
        let cursor = &mut self.cursors[index];
        cursor.last = Some((height, row.hash));
        cursor.next = Some(height + 1);
        Ok(true)
    }

    /// Sends the rollback of the cursor at `index` to the last block of its
    /// delivered branch that is still stored, and goes on after it.
    async fn rollback(&mut self, index: usize) -> Result<(), End> {
        let kept = self.kept(index).await?;
        let cursor = &self.cursors[index];
        let name = cursor.provider.name();
        let stored = cursor.provider.blocks(&cursor.db, kept, kept).await?;
        let data = format!("{{\"module\":\"{name}\",\"height\":{kept}}}");
        self.send(frame("rollback", None, &data)).await?;
        global().stream_message("rollback");
        let cursor = &mut self.cursors[index];
        cursor.last = stored.into_iter().next().map(|row| (row.height, row.hash));
        cursor.next = Some(kept + 1);
        Ok(())
    }

    /// The last block of the cursor's delivered branch that is still
    /// stored: followed back through the reorgs its module's poller saw, or,
    /// past what it saw, [`WINDOW`] blocks below the last delivered one,
    /// the deepest rollback the indexer walks.
    async fn kept(&self, index: usize) -> Result<i32, End> {
        let cursor = &self.cursors[index];
        let (mut height, mut hash) = cursor.last.clone().expect("a delivered block");
        let delivered = height;
        for _ in 0..MAX_FORKS {
            let stored = cursor.provider.blocks(&cursor.db, height, height).await?;
            if stored.first().is_some_and(|row| row.hash == hash) {
                return Ok(height.min(delivered - 1));
            }
            let kept = cursor
                .watch
                .history
                .lock()
                .expect("the history lock")
                .kept(height, &hash);
            match kept {
                Some((kept, Some(kept_hash))) => (height, hash) = (kept, kept_hash),
                Some((kept, None)) => return Ok(kept.min(delivered - 1)),
                None => break,
            }
        }
        let floor = cursor
            .provider
            .oldest(&cursor.db)
            .await?
            .map_or(i32::MIN, |oldest| oldest - 1);
        Ok((delivered - WINDOW).max(floor).min(delivered - 1))
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
        self.send(frame("heartbeat", None, &data)).await?;
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
