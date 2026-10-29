use {
    anyhow::{Context, Result, bail},
    opi_core::{BlockSource, BlockView, Module, metrics::ModuleMetrics},
    std::{convert::Infallible, sync::Arc, time::Duration},
    tokio::time::Instant,
};

/// Most blocks the scheduler rolls a module back in one step. A deeper
/// disagreement with the journal is an error.
pub const MAX_ROLLBACK_DEPTH: u32 = 1000;

/// With pruning on, a module prunes after applying every block whose height
/// is a multiple of this.
pub const PRUNE_EVERY: u32 = 100;

/// Follows the journal with `module` block by block until an error.
///
/// Every iteration reads the module's cursor, fetches the next block once the
/// journal's final tip reaches it, and then checks that the journal still
/// holds the cursor's hash. Checking after the fetch means a journal reorg
/// between the two reads shows up as a mismatch, so a fetched block always
/// extends the cursor. When the journal has no final block at the cursor's
/// height (catching up, or rebuilt), the module waits: the comparison runs
/// once the journal finalizes that height. On a mismatch the module rolls back
/// to the highest height where its stored hash equals the journal's, and the
/// iteration starts over. With no block to apply, it sleeps `poll` and starts
/// over.
///
/// `block_view` reads a whole block's records and inscription contents, so it
/// runs on tokio's blocking pool. `tip` and `block_info` are single-key redb
/// reads and run inline on the task.
///
/// Each cursor read records the module's indexed height and database pool in
/// the process metrics, and each applied block its height and apply time.
/// When `follow` ends, by an error, a panic or cancellation, the module's
/// height becomes unknown, so the process is not ready while it is down.
///
/// With `prune` set, every [`PRUNE_EVERY`] heights the module prunes its
/// history below `prune` blocks under the block it applied. A failed prune is
/// logged and the module goes on.
pub(crate) async fn follow(
    name: &'static str,
    module: &mut dyn Module,
    source: &Arc<dyn BlockSource>,
    poll: Duration,
    prune: Option<u32>,
) -> Result<Infallible> {
    let metrics = ClearHeightOnExit(opi_core::metrics::global().module(name));
    let metrics = &metrics.0;
    loop {
        let cursor = module.cursor().await.context("reading the cursor")?;
        metrics.set_height(cursor.as_ref().map(|cursor| cursor.height));
        if let Some(stats) = module.pool_stats() {
            metrics.set_pool(stats);
        }
        let next = cursor
            .as_ref()
            .map_or_else(|| module.start_height(), |cursor| cursor.height + 1);
        let block = if source.tip()?.is_some_and(|tip| tip >= next) {
            block_view(source, next).await?
        } else {
            None
        };

        if let Some(cursor) = cursor {
            match journal_hash(source.as_ref(), cursor.height)? {
                None => {
                    tracing::debug!(
                        module = name,
                        cursor = cursor.height,
                        "waiting for the journal to reach the cursor"
                    );
                    tokio::time::sleep(poll).await;
                    continue;
                }
                Some(hash) if hash != cursor.hash => {
                    let Some(target) =
                        agreement_height(module, source.as_ref(), cursor.height).await?
                    else {
                        tracing::debug!(
                            module = name,
                            cursor = cursor.height,
                            "the journal dropped below the rollback walk, retrying"
                        );
                        tokio::time::sleep(poll).await;
                        continue;
                    };
                    tracing::warn!(
                        module = name,
                        cursor = cursor.height,
                        target,
                        "the journal's hash at the cursor differs from the module's, rolling back"
                    );
                    module
                        .rollback_to(target)
                        .await
                        .with_context(|| format!("rolling back to {target}"))?;
                    continue;
                }
                Some(_) => {}
            }
        }

        match block {
            Some(block) => {
                let started = Instant::now();
                module
                    .apply(&block)
                    .await
                    .with_context(|| format!("applying block {next}"))?;
                metrics.observe_block(started.elapsed());
                metrics.set_height(Some(next));
                tracing::debug!(module = name, height = next, "applied");
                if let Some(keep) = prune.filter(|_| next % PRUNE_EVERY == 0) {
                    let below = (next + 1).saturating_sub(keep);
                    if let Err(err) = module.prune_below(below).await {
                        tracing::warn!(module = name, below, "pruning failed: {err:#}");
                    }
                }
            }
            None => tokio::time::sleep(poll).await,
        }
    }
}

/// A module's metrics handle that marks its height unknown when dropped.
struct ClearHeightOnExit(ModuleMetrics);

impl Drop for ClearHeightOnExit {
    fn drop(&mut self) {
        self.0.set_height(None);
    }
}

async fn block_view(source: &Arc<dyn BlockSource>, height: u32) -> Result<Option<BlockView>> {
    let source = source.clone();
    tokio::task::spawn_blocking(move || source.block_view(height))
        .await
        .with_context(|| format!("reading block {height} from the journal"))?
}

/// The journal's hash at `height`, or `None` above its final tip.
fn journal_hash(source: &dyn BlockSource, height: u32) -> Result<Option<String>> {
    if !source.tip()?.is_some_and(|tip| height <= tip) {
        return Ok(None);
    }
    Ok(source.block_info(height)?.map(|info| info.hash))
}

/// Walks down from `cursor` to the highest height where the module's stored
/// hash equals the journal's. When no height from `start_height()` up agrees,
/// the answer is `start_height() - 1`, which undoes every block; with a start
/// height of 0 that is an error. `None` means the journal has no final block
/// at a height the walk reached, and the caller retries later. A walk deeper
/// than `MAX_ROLLBACK_DEPTH` is an error.
async fn agreement_height(
    module: &mut dyn Module,
    source: &dyn BlockSource,
    cursor: u32,
) -> Result<Option<u32>> {
    let start = module.start_height();
    let mut height = cursor;
    loop {
        let Some(below) = height.checked_sub(1) else {
            bail!("no block from height 0 to {cursor} agrees with the journal");
        };
        if cursor - below > MAX_ROLLBACK_DEPTH {
            bail!(
                "every block from {height} to {cursor} differs from the journal, \
                 deeper than the {MAX_ROLLBACK_DEPTH} block rollback limit"
            );
        }
        if below < start {
            return Ok(Some(below));
        }
        let Some(hash) = journal_hash(source, below)? else {
            return Ok(None);
        };
        let stored = module
            .block_hash(below)
            .await
            .with_context(|| format!("reading the module's hash at {below}"))?;
        if stored == Some(hash) {
            return Ok(Some(below));
        }
        height = below;
    }
}
