//! Journal pruning of `run` with OPI_PRUNE: a background task deletes the
//! journal segments below the window.
//!
//! The window keeps raw events for the last OPI_PRUNE blocks below the final
//! tip, and records for the last OPI_PRUNE blocks below the lowest module
//! height. A segment goes once every height in it is below both, so a prune
//! deletes a few files and takes no time worth pacing.

use {
    anyhow::{Context, Result},
    opi_source::journal::{Journal, Pruned},
    std::time::Duration,
};

/// How often the pruner looks for segments to delete.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// The bounds a prune deletes below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Raw events go below this height.
    pub(crate) raw: u32,
    /// Records go below this height.
    pub(crate) records: u32,
}

/// The window that keeps `keep` blocks below `journal`'s final tip for raw
/// events, and `keep` blocks below `lowest`, the lowest module height, for
/// records. With `lowest` unknown no record is pruned. `None` before the
/// first finalized block.
pub(crate) fn window(journal: &Journal, lowest: Option<u32>, keep: u32) -> Result<Option<Window>> {
    let Some(tip) = journal.final_tip()? else {
        return Ok(None);
    };
    let raw = (tip + 1).saturating_sub(keep);
    let records = lowest
        .map_or(0, |lowest| (lowest + 1).saturating_sub(keep))
        .min(raw);
    Ok(Some(Window { raw, records }))
}

/// Prunes `journal` every [`PRUNE_INTERVAL`] until cancelled, keeping the
/// last `keep` blocks below the final tip and below the lowest height of the
/// `modules`. The module heights are those the scheduler records in the
/// process metrics; while any module's height is unknown (starting,
/// restarting or failed), nothing is pruned. A failed prune is logged and
/// retried at the next interval.
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

/// Prunes `journal` to its window once, on the blocking pool.
async fn prune_once(journal: &Journal, modules: &[&'static str], keep: u32) -> Result<Pruned> {
    let Some(window) = window(journal, lowest_module_height(modules), keep)? else {
        return Ok(Pruned::default());
    };
    let worker = journal.clone();
    let pruned = tokio::task::spawn_blocking(move || worker.prune(window.raw, window.records))
        .await
        .context("pruning task panicked")??;
    if !pruned.is_empty() {
        tracing::info!(
            raw_below = window.raw,
            records_below = window.records,
            segments = pruned.segments,
            tracked = pruned.tracked,
            journal_bytes = journal.disk_bytes().ok(),
            "pruned the journal"
        );
    }
    Ok(pruned)
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        opi_source::journal::{
            JournalOptions,
            testing::{self, Density},
        },
        ord::Chain,
    };

    const DENSITY: Density = Density {
        raw_events: 5,
        creations: 2,
        transfers: 1,
        content: 10,
    };

    fn journal(dir: &std::path::Path) -> Journal {
        let options = JournalOptions {
            segment: 100,
            ..JournalOptions::default()
        };
        Journal::open_with(dir, Chain::Regtest, false, &options).unwrap()
    }

    #[test]
    fn the_window_keeps_blocks_below_the_tip_and_the_lowest_module() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = journal(tmp.path());
        assert_eq!(window(&journal, Some(10), 1000).unwrap(), None);
        testing::fill(&journal, 0..2500, DENSITY).unwrap();
        assert_eq!(
            window(&journal, Some(2499), 1000).unwrap(),
            Some(Window {
                raw: 1500,
                records: 1500
            })
        );
        assert_eq!(
            window(&journal, Some(1800), 1000).unwrap(),
            Some(Window {
                raw: 1500,
                records: 801
            })
        );
        assert_eq!(
            window(&journal, None, 1000).unwrap(),
            Some(Window {
                raw: 1500,
                records: 0
            })
        );
    }

    /// The pruner deletes the segments below the window of the lowest module,
    /// and nothing while a module's height is unknown.
    #[tokio::test]
    async fn the_pruner_deletes_segments_below_the_lowest_module() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = journal(tmp.path());
        testing::fill(&journal, 0..2500, DENSITY).unwrap();
        let module = "prune_test_module";

        assert!(
            prune_once(&journal, &[module], 1000)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(journal.segment_starts().len(), 25);

        opi_core::metrics::global()
            .module(module)
            .set_height(Some(1800));
        let pruned = prune_once(&journal, &[module], 1000).await.unwrap();
        assert_eq!(pruned.segments, 8);
        assert_eq!(journal.segment_starts().first(), Some(&800));
        assert_eq!(journal.pruned_below().unwrap(), 800);
    }
}
