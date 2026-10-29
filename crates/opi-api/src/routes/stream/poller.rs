//! The pollers: one per module and process, shared by every subscriber of
//! that module. A poller reads the module's highest block-hash row every
//! STREAM_POLL_MS; when it changes, the poller reads the top [`WINDOW`]
//! rows, records every block hash a reorg replaced together with the block
//! the reorg kept, and tells the subscribers through a `watch` channel. A
//! poller starts with the first subscriber of its module and stops after
//! the last one has gone, or at shutdown.

use {
    super::{StreamStop, provider::Provider},
    crate::db::Db,
    std::{
        collections::{BTreeMap, HashMap, VecDeque},
        sync::{Arc, Mutex},
        time::Duration,
    },
    tokio::sync::watch,
};

/// The rows a poller keeps, and the deepest reorg it records: the deepest
/// rollback the indexer walks.
pub const WINDOW: i32 = 1000;

/// The most replaced block hashes a poller remembers.
const REPLACED_CAP: usize = 100_000;

/// A module's highest indexed block as its poller last read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tip {
    /// The highest height of the block-hash table; `None` before the first
    /// read and for an empty table.
    pub height: Option<i32>,
    /// Counts the changes the poller has seen.
    pub version: u64,
}

/// The block a reorg kept: its height, and its hash when the poller held
/// that row.
pub type Kept = (i32, Option<String>);

/// A module's recent block hashes and the hashes reorgs replaced.
#[derive(Debug, Default)]
pub struct History {
    /// The top rows of the block-hash table, by height.
    window: BTreeMap<i32, String>,
    /// Each replaced `(height, hash)` and the block its reorg kept.
    replaced: HashMap<(i32, String), Kept>,
    /// The keys of `replaced`, oldest first.
    order: VecDeque<(i32, String)>,
}

impl History {
    /// Takes the rows `fetched` of the heights from `low` up as the new
    /// window. A cached row from `low` up that `fetched` lacks or holds
    /// with another hash was replaced; the lowest such height less one is
    /// the block the reorg kept. Rows below `low` leave the window as they
    /// are (pruning deletes them).
    pub fn update(&mut self, low: i32, fetched: BTreeMap<i32, String>) {
        let first = self
            .window
            .range(low..)
            .find(|(height, hash)| fetched.get(height) != Some(hash))
            .map(|(height, _)| *height);
        if let Some(first) = first {
            let kept = first - 1;
            let kept_hash = fetched.get(&kept).cloned();
            let replaced: Vec<(i32, String)> = self
                .window
                .range(first..)
                .filter(|(height, hash)| fetched.get(height) != Some(hash))
                .map(|(height, hash)| (*height, hash.clone()))
                .collect();
            for key in replaced {
                if self
                    .replaced
                    .insert(key.clone(), (kept, kept_hash.clone()))
                    .is_none()
                {
                    self.order.push_back(key);
                }
            }
            while self.order.len() > REPLACED_CAP {
                if let Some(key) = self.order.pop_front() {
                    self.replaced.remove(&key);
                }
            }
        }
        self.window = fetched;
    }

    /// The block kept by the reorg that replaced `hash` at `height`, if
    /// this poller saw it.
    pub fn kept(&self, height: i32, hash: &str) -> Option<Kept> {
        self.replaced.get(&(height, hash.to_string())).cloned()
    }

    /// The highest row of the window.
    fn top(&self) -> Option<(i32, &String)> {
        self.window.iter().next_back().map(|(h, hash)| (*h, hash))
    }
}

/// A subscriber's view of a module's poller.
#[derive(Clone)]
pub struct Watch {
    pub tip: watch::Receiver<Tip>,
    pub history: Arc<Mutex<History>>,
}

type Slot = (watch::Sender<Tip>, Arc<Mutex<History>>);

/// The pollers of a process.
pub struct Pollers {
    slots: Mutex<HashMap<Provider, Slot>>,
    poll: Duration,
    stop: StreamStop,
}

impl Pollers {
    pub fn new(poll: Duration, stop: StreamStop) -> Arc<Pollers> {
        Arc::new(Pollers {
            slots: Mutex::default(),
            poll,
            stop,
        })
    }

    /// A view of `provider`'s poller over `db`, starting the poller when
    /// none runs.
    pub fn watch(self: &Arc<Self>, provider: Provider, db: &Db) -> Watch {
        let mut slots = self.slots.lock().expect("the pollers lock");
        if let Some((tip, history)) = slots.get(&provider) {
            return Watch {
                tip: tip.subscribe(),
                history: history.clone(),
            };
        }
        let (tip, receiver) = watch::channel(Tip::default());
        let history = Arc::new(Mutex::new(History::default()));
        slots.insert(provider, (tip.clone(), history.clone()));
        tokio::spawn(self.clone().run(provider, db.clone(), tip, history.clone()));
        Watch {
            tip: receiver,
            history,
        }
    }

    async fn run(
        self: Arc<Self>,
        provider: Provider,
        db: Db,
        tip: watch::Sender<Tip>,
        history: Arc<Mutex<History>>,
    ) {
        let mut stop = self.stop.clone();
        let mut last: Option<(i32, String)> = None;
        let mut failing = false;
        loop {
            match poll(provider, &db, &mut last, &history).await {
                Ok(Some(height)) => {
                    tip.send_modify(|tip| {
                        tip.height = height;
                        tip.version += 1;
                    });
                    failing = false;
                }
                Ok(None) => failing = false,
                Err(err) if !failing => {
                    tracing::warn!(
                        "stream: reading the {} block hashes: {err}",
                        provider.name()
                    );
                    failing = true;
                }
                Err(_) => {}
            }
            let stopped = tokio::select! {
                () = tokio::time::sleep(self.poll) => false,
                () = stop.stopped() => true,
            };
            // Under the lock, so a subscriber either takes this poller
            // before it stops or starts a new one.
            let mut slots = self.slots.lock().expect("the pollers lock");
            if stopped || tip.receiver_count() == 0 {
                slots.remove(&provider);
                return;
            }
        }
    }
}

/// Reads `provider`'s highest row and, when it differs from `last`, the
/// window below it. Answers the new highest height when it changed.
async fn poll(
    provider: Provider,
    db: &Db,
    last: &mut Option<(i32, String)>,
    history: &Mutex<History>,
) -> Result<Option<Option<i32>>, crate::reply::ApiError> {
    let tip = provider.tip(db).await?;
    if tip == *last {
        return Ok(None);
    }
    let old = history
        .lock()
        .expect("the history lock")
        .top()
        .map(|(h, _)| h);
    let Some(top) = tip.as_ref().map(|(h, _)| *h).or(old) else {
        *last = tip;
        return Ok(Some(None));
    };
    let low = top.min(old.unwrap_or(top)) - WINDOW + 1;
    let rows = match &tip {
        Some((height, _)) => provider.blocks(db, low, *height).await?,
        None => Vec::new(),
    };
    let fetched: BTreeMap<i32, String> =
        rows.into_iter().map(|row| (row.height, row.hash)).collect();
    let mut history = history.lock().expect("the history lock");
    history.update(low, fetched);
    // the window is one snapshot, which may hold a block above `tip`
    *last = history.top().map(|(h, hash)| (h, hash.clone()));
    Ok(Some(last.as_ref().map(|(h, _)| *h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[(i32, &str)]) -> BTreeMap<i32, String> {
        rows.iter()
            .map(|(h, hash)| (*h, hash.to_string()))
            .collect()
    }

    #[test]
    fn a_reorg_records_each_replaced_hash_with_the_kept_block() {
        let mut history = History::default();
        history.update(1, rows(&[(1, "a"), (2, "b"), (3, "c"), (4, "d")]));
        history.update(1, rows(&[(1, "a"), (2, "b"), (3, "x"), (4, "y"), (5, "z")]));
        assert_eq!(history.kept(3, "c"), Some((2, Some("b".into()))));
        assert_eq!(history.kept(4, "d"), Some((2, Some("b".into()))));
        assert_eq!(history.kept(3, "x"), None);
        // a deeper reorg of the new branch
        history.update(1, rows(&[(1, "a"), (2, "q")]));
        assert_eq!(history.kept(2, "b"), Some((1, Some("a".into()))));
        assert_eq!(history.kept(4, "y"), Some((1, Some("a".into()))));
        assert_eq!(history.kept(3, "c"), Some((2, Some("b".into()))));
    }

    #[test]
    fn rows_below_the_window_leave_without_a_reorg() {
        let mut history = History::default();
        history.update(1, rows(&[(1, "a"), (2, "b"), (3, "c")]));
        history.update(3, rows(&[(3, "c"), (4, "d")]));
        assert_eq!(history.kept(1, "a"), None);
        assert_eq!(history.kept(2, "b"), None);
        assert_eq!(history.top(), Some((4, &"d".to_string())));
    }

    #[test]
    fn a_reorg_below_the_window_keeps_an_unknown_hash() {
        let mut history = History::default();
        history.update(5, rows(&[(5, "a"), (6, "b")]));
        history.update(5, rows(&[(5, "x"), (6, "y")]));
        assert_eq!(history.kept(5, "a"), Some((4, None)));
    }
}
