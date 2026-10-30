//! The blocks the streams share: a block's `block` frame up to its id line
//! (its head and its pages of events), rendered once per module, height
//! and hash and sent to every subscriber, so a block's events are read
//! once however many streams deliver it. A block several streams ask for at
//! once is rendered by one of them while the others wait for it. The
//! rendered blocks are kept, oldest out first, up to [`BUDGET`] bytes.

use {
    super::provider::Provider,
    crate::reply::ApiError,
    axum::body::Bytes,
    std::{
        collections::{HashMap, VecDeque},
        future::Future,
        sync::{Arc, Mutex},
    },
    tokio::sync::OnceCell,
};

/// The bytes of rendered blocks kept.
pub const BUDGET: usize = 64 << 20;

/// A block's chunks, up to its id line.
pub type Rendered = Arc<[Bytes]>;

type Key = (Provider, i32, String);

pub struct Blocks {
    budget: usize,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    /// The rendered blocks' keys, oldest first.
    order: VecDeque<Key>,
    bytes: usize,
}

struct Entry {
    cell: Arc<OnceCell<Rendered>>,
    /// The rendered block's bytes; `None` while it renders.
    size: Option<usize>,
}

impl Blocks {
    pub fn new(budget: usize) -> Arc<Blocks> {
        Arc::new(Blocks {
            budget,
            cache: Mutex::default(),
        })
    }

    /// The block of `provider` at `height` with `hash`, rendered by
    /// `render` unless it is kept or another stream renders it.
    pub async fn get<F, Fut>(
        &self,
        provider: Provider,
        height: i32,
        hash: &str,
        render: F,
    ) -> Result<Rendered, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Bytes>, ApiError>>,
    {
        let key = (provider, height, hash.to_string());
        let cell = self
            .lock()
            .entries
            .entry(key.clone())
            .or_insert_with(|| Entry {
                cell: Arc::default(),
                size: None,
            })
            .cell
            .clone();
        let rendered = cell
            .get_or_try_init(|| async { render().await.map(Rendered::from) })
            .await
            .map(Arc::clone);
        let mut cache = self.lock();
        let ours = cache
            .entries
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell));
        match &rendered {
            Ok(chunks) if ours => cache.keep(key, chunks, self.budget),
            Err(_) if ours && !cell.initialized() => {
                cache.entries.remove(&key);
            }
            _ => {}
        }
        rendered
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("the blocks lock")
    }

    #[cfg(test)]
    fn kept(&self) -> Vec<i32> {
        self.lock()
            .order
            .iter()
            .map(|(_, height, _)| *height)
            .collect()
    }
}

impl Cache {
    /// Counts the rendered block of `key` once, then drops the oldest
    /// blocks past `budget`, never the newest.
    fn keep(&mut self, key: Key, chunks: &Rendered, budget: usize) {
        let entry = self.entries.get_mut(&key).expect("the block's entry");
        if entry.size.is_some() {
            return;
        }
        let size = chunks.iter().map(Bytes::len).sum();
        entry.size = Some(size);
        self.bytes += size;
        self.order.push_back(key);
        while self.bytes > budget && self.order.len() > 1 {
            let oldest = self.order.pop_front().expect("a kept block");
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.size.unwrap_or(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::sync::atomic::{AtomicUsize, Ordering},
    };

    fn chunks(text: &'static str) -> Vec<Bytes> {
        vec![Bytes::from_static(text.as_bytes())]
    }

    #[tokio::test]
    async fn a_block_renders_once_for_every_stream() {
        let blocks = Blocks::new(BUDGET);
        let renders = AtomicUsize::new(0);
        let render = || async {
            renders.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(chunks("head"))
        };
        let (a, b) = tokio::join!(
            blocks.get(Provider::Brc20, 1, "h", render),
            blocks.get(Provider::Brc20, 1, "h", render),
        );
        assert_eq!(a.unwrap()[0], "head");
        assert_eq!(b.unwrap()[0], "head");
        blocks.get(Provider::Brc20, 1, "h", render).await.unwrap();
        assert_eq!(renders.load(Ordering::SeqCst), 1);
        // another hash at the height is another block
        blocks.get(Provider::Brc20, 1, "x", render).await.unwrap();
        assert_eq!(renders.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_render_is_not_kept() {
        let blocks = Blocks::new(BUDGET);
        let failed = blocks
            .get(Provider::Sns, 1, "h", || async {
                Err(ApiError::from(anyhow::anyhow!("down")))
            })
            .await;
        assert!(failed.is_err());
        let rendered = blocks
            .get(Provider::Sns, 1, "h", || async { Ok(chunks("ok")) })
            .await
            .unwrap();
        assert_eq!(rendered[0], "ok");
    }

    #[tokio::test]
    async fn the_oldest_blocks_leave_past_the_budget() {
        let blocks = Blocks::new(8);
        for height in 1..=3 {
            blocks
                .get(Provider::Brc20, height, "h", || async {
                    Ok(chunks("four"))
                })
                .await
                .unwrap();
        }
        assert_eq!(blocks.kept(), [2, 3]);
        // a block above the whole budget is kept until the next one
        blocks
            .get(Provider::Brc20, 4, "h", || async {
                Ok(chunks("0123456789"))
            })
            .await
            .unwrap();
        assert_eq!(blocks.kept(), [4]);
    }
}
