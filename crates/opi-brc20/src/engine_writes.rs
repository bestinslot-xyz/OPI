//! A process-wide guard around the engine's writes to its database.
//!
//! brc20-prog writes a commit (`brc20_commit_to_database`, and the commit
//! that ends `brc20_reorg`) as a series of separate RocksDB writes, block
//! hashes first. A process that exits partway leaves a height and a hash with
//! no state behind them, which the next start cannot detect. Every such write
//! runs under this guard, and an exit that must not cut one short takes the
//! guard first (`wait_until_free`). `brc20_initialise` runs under it too; it
//! leaves its genesis block for the next commit.

use std::{future::Future, time::Duration};

use tokio::sync::{Mutex, MutexGuard};

static ENGINE_WRITES: Mutex<()> = Mutex::const_new(());

/// Runs `write`, an engine call that writes the engine's database, under the
/// guard.
pub async fn guarded<F: Future>(write: F) -> F::Output {
    let _guard = ENGINE_WRITES.lock().await;
    write.await
}

/// Takes the guard once the engine write in progress has finished, or
/// `None` after `limit`. While the guard is held no engine write starts.
pub async fn wait_until_free(limit: Duration) -> Option<MutexGuard<'static, ()>> {
    tokio::time::timeout(limit, ENGINE_WRITES.lock()).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_guard_waits_for_the_write_in_progress() {
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let write = tokio::spawn(guarded(async move {
            started.send(()).unwrap();
            finished.await.ok();
        }));
        wait_started.await.unwrap();
        assert!(wait_until_free(Duration::from_millis(50)).await.is_none());
        finish.send(()).unwrap();
        let guard = wait_until_free(Duration::from_secs(5)).await;
        assert!(guard.is_some());
        drop(guard);
        write.await.unwrap();
    }
}
