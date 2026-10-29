//! Journal pruning of `run` with OPI_PRUNE set.

use {
    anyhow::{Context, Result},
    opi_source::journal::{Journal, PRUNE_BATCH},
    std::time::Duration,
};

/// How often the pruner looks for journal rows to delete.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// Prunes `journal` every [`PRUNE_INTERVAL`] until cancelled, keeping the
/// last `keep` blocks below the final tip and below the lowest height of the
/// `modules`. The module heights are those the scheduler records in the
/// process metrics; while any module's height is unknown (starting,
/// restarting or failed), only raw events are pruned. A failed prune is
/// logged and retried at the next interval.
pub(crate) async fn prune_journal(journal: Journal, modules: Vec<&'static str>, keep: u32) {
    let mut interval = tokio::time::interval(PRUNE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(err) = prune_once(&journal, &modules, keep).await {
            tracing::warn!("pruning the journal failed, retrying later: {err:#}");
        }
    }
}

/// Prunes in steps of at most [`PRUNE_BATCH`] heights per table until
/// caught up, each step on the blocking pool, so a cancel lands between
/// steps.
async fn prune_once(journal: &Journal, modules: &[&'static str], keep: u32) -> Result<()> {
    loop {
        let Some(tip) = journal.final_tip()? else {
            return Ok(());
        };
        let raw_below = (tip + 1).saturating_sub(keep);
        let records_below = match lowest_module_height(modules) {
            Some(lowest) => (lowest + 1).saturating_sub(keep),
            None => 0,
        };
        let raw_step = match journal.raw_floor()? {
            Some(raw_floor) => raw_below.min(raw_floor.saturating_add(PRUNE_BATCH)),
            None => raw_below,
        };
        let floor = journal.pruned_below()?;
        let records_step = records_below
            .min(floor.saturating_add(PRUNE_BATCH))
            .max(floor);
        let worker = journal.clone();
        let pruned = tokio::task::spawn_blocking(move || worker.prune(raw_step, records_step))
            .await
            .context("pruning task panicked")??;
        if !pruned.is_empty() {
            tracing::debug!(
                raw_below = raw_step,
                records_below = records_step,
                ?pruned,
                "pruned"
            );
        }
        if raw_step == raw_below && records_step >= records_below {
            return Ok(());
        }
    }
}

/// The lowest height the scheduler recorded for `modules`, or `None` when any
/// of them has none.
fn lowest_module_height(modules: &[&'static str]) -> Option<u32> {
    let heights = opi_core::metrics::global().module_heights();
    modules
        .iter()
        .map(|name| {
            heights
                .iter()
                .find(|(module, _)| module == name)
                .and_then(|(_, height)| *height)
                .and_then(|height| u32::try_from(height).ok())
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min()
}

/// Rewrites the journal file at `path` without the pages pruning freed.
pub(crate) fn compact_journal(path: &std::path::Path) -> Result<()> {
    let before = std::fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    tracing::info!(path = %path.display(), bytes = before, "compacting the journal");
    Journal::compact(path).with_context(|| format!("compacting {}", path.display()))?;
    let after = std::fs::metadata(path)?.len();
    tracing::info!(
        bytes = after,
        freed = before.saturating_sub(after),
        "compacted"
    );
    Ok(())
}
