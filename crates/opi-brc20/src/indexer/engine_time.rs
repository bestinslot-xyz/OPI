//! The time a block spends in the BRC2.0 engine, for the process metrics'
//! `opi_engine_seconds`.
//!
//! [`timed`] adds each engine call's duration to one counter and counts the
//! call; a block starts with [`EngineBlock::start`], which clears both, and
//! [`EngineBlock::finish`] records the sum for a block that called the
//! engine. brc20 indexes one block at a time on one thread, so the counters
//! hold the current block's calls alone.

use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static ENGINE_NANOS: AtomicU64 = AtomicU64::new(0);
static ENGINE_CALLS: AtomicU64 = AtomicU64::new(0);

/// Awaits `call`, counting its duration as engine time of the current block.
pub(crate) async fn timed<T>(call: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let output = call.await;
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    ENGINE_NANOS.fetch_add(nanos, Ordering::Relaxed);
    ENGINE_CALLS.fetch_add(1, Ordering::Relaxed);
    output
}

/// The engine time of one block.
pub(crate) struct EngineBlock;

impl EngineBlock {
    pub(crate) fn start() -> EngineBlock {
        ENGINE_NANOS.store(0, Ordering::Relaxed);
        ENGINE_CALLS.store(0, Ordering::Relaxed);
        EngineBlock
    }

    /// Records the block's engine time, unless the block made no engine
    /// call.
    pub(crate) fn finish(self) {
        let nanos = ENGINE_NANOS.swap(0, Ordering::Relaxed);
        if ENGINE_CALLS.swap(0, Ordering::Relaxed) > 0 {
            engine_seconds().observe(Duration::from_nanos(nanos));
        }
    }
}

fn engine_seconds() -> &'static opi_core::metrics::AtomicHistogram {
    static HISTOGRAM: std::sync::OnceLock<opi_core::metrics::AtomicHistogram> =
        std::sync::OnceLock::new();
    HISTOGRAM.get_or_init(|| opi_core::metrics::global().engine_seconds("brc20"))
}
