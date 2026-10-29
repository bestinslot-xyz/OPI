//! A route answer that depends only on the indexed state: computed once per
//! indexed block and served until the tip moves.
//!
//! The key is the module's tip, the last row of `<module>_block_hashes`:
//! its height and block hash, so a reorg that indexes another block at a
//! cached height computes anew. A request reads the tip and serves the
//! cached answer when it was computed at that tip. Otherwise it waits its
//! turn to compute: one computation runs at a time, and a request that
//! finds, once its turn comes, an answer at the tip it then reads serves
//! that answer. Concurrent requests for an uncached tip so share one
//! computation. The computation runs as its own task, so a client that goes
//! away leaves it to finish for the requests behind it.
//!
//! A computation reads the tip and the answer in one `REPEATABLE READ READ
//! ONLY` transaction, so the answer is the state at that tip. It is cached
//! under that tip when the computation reports it settled; one that read
//! rows the indexer wrote for a block whose hash is not committed yet is
//! served once and not cached.

use {
    crate::{
        config::Module,
        db::Db,
        reply::{ApiError, Reply},
    },
    futures::future::BoxFuture,
    sqlx::{PgConnection, PgExecutor},
    std::sync::{Arc, Mutex},
};

/// A tip: the last indexed height and its block hash, `None` before the
/// first block.
pub type Tip = Option<(i32, String)>;

/// A route's computation, run in the cache's read-only transaction: the
/// answer at the tip height it is given (`None` before the first block),
/// and whether that answer is settled at that tip, and so cacheable.
pub type Compute =
    for<'c> fn(&'c mut PgConnection, Option<i32>) -> BoxFuture<'c, Result<(Reply, bool), ApiError>>;

/// A route's cached answer; clones share it.
#[derive(Clone)]
pub struct TipCache {
    db: Db,
    module: Module,
    compute: Compute,
    cached: Arc<Mutex<Option<(Tip, Reply)>>>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl TipCache {
    pub fn new(db: Db, module: Module, compute: Compute) -> TipCache {
        TipCache {
            db,
            module,
            compute,
            cached: Arc::new(Mutex::new(None)),
            turn: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// The answer at the current tip.
    pub async fn answer(&self) -> Result<Reply, ApiError> {
        let pool = self.db.pool()?;
        if let Some(reply) = self.cached_at(&tip(pool, self.module).await?) {
            return Ok(reply);
        }
        let turn = self.turn.clone().lock_owned().await;
        if let Some(reply) = self.cached_at(&tip(pool, self.module).await?) {
            return Ok(reply);
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let _turn = turn;
            cache.compute().await
        })
        .await?
    }

    fn cached_at(&self, at: &Tip) -> Option<Reply> {
        match &*self
            .cached
            .lock()
            .expect("the cache lock is never poisoned")
        {
            Some((tip, reply)) if tip == at => Some(reply.clone()),
            _ => None,
        }
    }

    async fn compute(&self) -> Result<Reply, ApiError> {
        let mut tx = self
            .db
            .pool()?
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await?;
        let at = tip(&mut *tx, self.module).await?;
        let (reply, settled) =
            (self.compute)(&mut tx, at.as_ref().map(|(height, _)| *height)).await?;
        tx.commit().await?;
        if settled {
            *self
                .cached
                .lock()
                .expect("the cache lock is never poisoned") = Some((at, reply.clone()));
        }
        Ok(reply)
    }
}

/// `module`'s tip.
async fn tip<'e>(executor: impl PgExecutor<'e>, module: Module) -> anyhow::Result<Tip> {
    let row = match module {
        Module::Brc20 => sqlx::query!(
            "SELECT block_height, block_hash FROM brc20_block_hashes ORDER BY block_height DESC LIMIT 1;"
        )
        .fetch_optional(executor)
        .await?
        .map(|row| (row.block_height, row.block_hash)),
        Module::Sns => sqlx::query!(
            "SELECT block_height, block_hash FROM sns_block_hashes ORDER BY block_height DESC LIMIT 1;"
        )
        .fetch_optional(executor)
        .await?
        .map(|row| (row.block_height, row.block_hash)),
        Module::Bitmap | Module::Runes => {
            anyhow::bail!("no cached route reads the {} tip", module.name())
        }
    };
    Ok(row)
}
