use {
    anyhow::{Context, Result, bail},
    opi_core::{BlockSource, Module},
    std::{convert::Infallible, time::Duration},
};

/// Most blocks the scheduler rolls a module back in one step. A deeper
/// disagreement with the journal is an error.
pub const MAX_ROLLBACK_DEPTH: u32 = 1000;

/// Follows the journal with `module` block by block until an error.
///
/// Every iteration reads the module's cursor, fetches the next block once the
/// journal's final tip reaches it, and then checks that the journal still
/// holds the cursor's hash. Checking after the fetch means a journal reorg
/// between the two reads shows up as a mismatch, so a fetched block always
/// extends the cursor. While the journal's final tip is below the cursor
/// (catching up, or rebuilt), the module waits: the comparison runs once the
/// journal finalizes the cursor's height. On a mismatch the module rolls back
/// to the highest height where its stored hash equals the journal's, and the
/// iteration starts over. With no block to apply, it sleeps `poll` and starts
/// over.
///
/// `source` is called inline on the task: its reads are short local redb
/// transactions, and the runtime is multi-threaded, so other modules keep
/// running meanwhile.
pub(crate) async fn follow(
    name: &'static str,
    module: &mut dyn Module,
    source: &dyn BlockSource,
    poll: Duration,
) -> Result<Infallible> {
    loop {
        let cursor = module.cursor().await.context("reading the cursor")?;
        let next = cursor
            .as_ref()
            .map_or_else(|| module.start_height(), |cursor| cursor.height + 1);
        let block = if source.tip()?.is_some_and(|tip| tip >= next) {
            source.block_view(next)?
        } else {
            None
        };

        if let Some(cursor) = &cursor
            && !source.tip()?.is_some_and(|tip| tip >= cursor.height)
        {
            tracing::debug!(
                module = name,
                cursor = cursor.height,
                "waiting for the journal to reach the cursor"
            );
            tokio::time::sleep(poll).await;
            continue;
        }

        if let Some(cursor) = cursor
            && journal_hash(source, cursor.height)?.as_deref() != Some(cursor.hash.as_str())
        {
            let target = agreement_height(module, source, cursor.height).await?;
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

        match block {
            Some(block) => {
                module
                    .apply(&block)
                    .await
                    .with_context(|| format!("applying block {next}"))?;
                tracing::debug!(module = name, height = next, "applied");
            }
            None => tokio::time::sleep(poll).await,
        }
    }
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
/// the answer is `start_height() - 1`, which undoes every block.
async fn agreement_height(
    module: &mut dyn Module,
    source: &dyn BlockSource,
    cursor: u32,
) -> Result<u32> {
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
            return Ok(below);
        }
        if let Some(hash) = journal_hash(source, below)?
            && module
                .block_hash(below)
                .await
                .with_context(|| format!("reading the module's hash at {below}"))?
                == Some(hash)
        {
            return Ok(below);
        }
        height = below;
    }
}
