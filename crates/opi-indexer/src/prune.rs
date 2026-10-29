//! Journal pruning with OPI_PRUNE: the catch-up `run` performs before it
//! starts the source, `run`'s background pruner, and the prune of
//! `compact-journal`.
//!
//! Every path prunes the same window: raw events below the final tip minus
//! the kept blocks, and transfer records, per-block rune rows and consumed
//! inscription content below the lowest module height minus the kept blocks.
//! Each step is one [`Journal::prune_step`], whose write transaction the row
//! budget [`PRUNE_STEP_ROWS`] keeps short.

use {
    crate::config::ModuleConfigs,
    anyhow::{Context, Result, anyhow, ensure},
    opi_source::journal::{Journal, PRUNE_STEP_ROWS, Pruned, is_in_use},
    ord::Chain,
    sqlx::{
        Connection, PgConnection,
        postgres::{PgConnectOptions, PgSslMode},
    },
    std::{
        path::Path,
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    },
    tokio::signal::unix::{SignalKind, signal},
};

/// How often the background pruner looks for journal rows to delete.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// Shortest pause between two steps of the background pruner, so the
/// stager's write transactions take the journal in between.
const STEP_PAUSE: Duration = Duration::from_millis(50);

/// How often the catch-up logs its progress.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// How long reading one module's height may take.
const HEIGHT_TIMEOUT: Duration = Duration::from_secs(30);

/// The metrics fill the catch-up runs as, so readiness answers 503 and
/// `/metrics` shows its progress.
const FILL: (&str, &str) = ("journal", "journal_prune");

/// Set by SIGINT or SIGTERM once [`watch_signals`] has run.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

/// Whether SIGINT or SIGTERM arrived since [`watch_signals`] ran.
pub(crate) fn signalled() -> bool {
    SIGNALLED.load(Ordering::SeqCst)
}

/// Records every SIGINT and SIGTERM in [`signalled`] for the rest of the
/// process, on a thread of its own. The handlers are installed when it
/// returns.
pub(crate) fn watch_signals() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (mut sigint, mut sigterm) = {
        let _entered = runtime.enter();
        (
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        )
    };
    std::thread::Builder::new()
        .name("opi-signals".into())
        .spawn(move || {
            runtime.block_on(async move {
                loop {
                    tokio::select! {
                        _ = sigint.recv() => {}
                        _ = sigterm.recv() => {}
                    }
                    SIGNALLED.store(true, Ordering::SeqCst);
                }
            })
        })
        .context("spawning the signal thread")?;
    Ok(())
}

/// Resolves once [`signalled`] turns true.
pub(crate) async fn wait_signalled() {
    while !signalled() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The bounds a prune deletes below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Raw events go below this height.
    pub(crate) raw: u32,
    /// Transfer records, per-block rune rows and consumed info rows go
    /// below this height.
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

/// Heights left to prune below `window`, raw events and records.
fn remaining(journal: &Journal, window: Window) -> Result<(u32, u32)> {
    let raw = window
        .raw
        .saturating_sub(journal.raw_floor()?.unwrap_or(window.raw));
    let records = window.records.saturating_sub(journal.pruned_below()?);
    Ok((raw, records))
}

/// How a catch-up ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatchUp {
    /// The journal holds nothing to prune below the window.
    Done,
    /// `stop` held between two steps. Every step committed whole, so the
    /// journal is valid and the next catch-up resumes from its floors.
    Interrupted,
}

/// Prunes `journal` to `window` in steps of at most `max_rows` rows per
/// table, each committing on its own, until done or until `stop` returns
/// true between two steps. It runs as the [`FILL`] fill and logs the heights
/// done and remaining per table every [`PROGRESS_EVERY`] at info.
pub(crate) fn catch_up(
    journal: &Journal,
    window: Window,
    max_rows: u64,
    stop: &dyn Fn() -> bool,
) -> Result<CatchUp> {
    let (raw_total, records_total) = remaining(journal, window)?;
    if raw_total == 0 && records_total == 0 {
        return Ok(CatchUp::Done);
    }
    let fill = opi_core::metrics::global().fill(FILL.0, FILL.1);
    fill.target(i64::from(raw_total) + i64::from(records_total));
    fill.progress(0);
    tracing::info!(
        raw_events_below = window.raw,
        raw_event_heights = raw_total,
        records_below = window.records,
        record_heights = records_total,
        "pruning the journal to the OPI_PRUNE window"
    );
    let started = Instant::now();
    let mut logged = started;
    let mut pruned = Pruned::default();
    let (mut steps, mut longest) = (0u64, Duration::ZERO);
    loop {
        let (raw_left, records_left) = remaining(journal, window)?;
        let (raw_done, records_done) = (
            raw_total.saturating_sub(raw_left),
            records_total.saturating_sub(records_left),
        );
        fill.progress(i64::from(raw_done) + i64::from(records_done));
        if stop() {
            tracing::info!(
                raw_event_heights_left = raw_left,
                record_heights_left = records_left,
                elapsed_secs = started.elapsed().as_secs(),
                "journal prune stopped by a signal; the next start resumes it"
            );
            return Ok(CatchUp::Interrupted);
        }
        if logged.elapsed() >= PROGRESS_EVERY {
            tracing::info!(
                raw_event_heights_done = raw_done,
                raw_event_heights_left = raw_left,
                record_heights_done = records_done,
                record_heights_left = records_left,
                elapsed_secs = started.elapsed().as_secs(),
                "pruning the journal"
            );
            logged = Instant::now();
        }
        let step_started = Instant::now();
        let step = journal.prune_step(window.raw, window.records, max_rows)?;
        longest = longest.max(step_started.elapsed());
        steps += 1;
        pruned.add(step.pruned);
        if step.done {
            tracing::info!(
                raw_events = pruned.raw_events,
                transfers = pruned.transfers,
                infos = pruned.infos,
                rune_rows = pruned.rune_rows,
                steps,
                longest_step_ms = longest.as_millis(),
                elapsed_secs = started.elapsed().as_secs(),
                "pruned the journal to the OPI_PRUNE window"
            );
            return Ok(CatchUp::Done);
        }
    }
}

/// Opens the existing journal at `path`, refusing while another open, such
/// as `run`, holds it.
fn open_existing(path: &Path, chain: Chain, runes: bool) -> Result<Journal> {
    ensure!(path.exists(), "no journal at {}", path.display());
    Journal::open(path, chain, runes).map_err(|err| in_use(err, path))
}

fn in_use(err: anyhow::Error, path: &Path) -> anyhow::Error {
    if is_in_use(&err) {
        anyhow!(
            "the journal at {} is in use; stop `opi-indexer run` first",
            path.display()
        )
    } else {
        err
    }
}

/// `run`'s catch-up: prunes the journal at `path` to the window of `keep`
/// blocks below its final tip and below `lowest`, closes it, and only then
/// calls `start`, which starts ord and the modules. Nothing else holds the
/// journal while it prunes. `None` when a signal stopped the catch-up;
/// `start` has not run then. A journal that does not exist yet has nothing
/// to prune.
pub(crate) fn catch_up_then<T>(
    path: &Path,
    chain: Chain,
    runes: bool,
    lowest: Option<u32>,
    keep: u32,
    stop: &dyn Fn() -> bool,
    start: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    if path.exists() {
        let journal = open_existing(path, chain, runes)?;
        if let Some(window) = window(&journal, lowest, keep)?
            && catch_up(&journal, window, PRUNE_STEP_ROWS, stop)? == CatchUp::Interrupted
        {
            return Ok(None);
        }
    }
    start().map(Some)
}

/// What `compact-journal` prunes with before it compacts.
pub(crate) struct OfflinePrune<'a> {
    pub(crate) chain: Chain,
    pub(crate) runes: bool,
    pub(crate) keep: u32,
    pub(crate) modules: &'a [ModuleDb],
}

/// `compact-journal`: with `prune`, prunes the journal at `path` to the
/// window of `prune.keep` blocks below its final tip and below the lowest
/// height read from the module databases, then compacts it. Refuses when a
/// module's height cannot be read, and while another open holds the journal.
/// A signal between two prune steps ends it without compacting.
pub(crate) fn compact_command(
    path: &Path,
    prune: Option<OfflinePrune>,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    if let Some(prune) = prune {
        let journal = open_existing(path, prune.chain, prune.runes)?;
        let lowest = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(lowest_height(prune.modules))
            .context(
                "compact-journal prunes to OPI_PRUNE first, from every enabled module's height",
            )?;
        if let Some(window) = window(&journal, Some(lowest), prune.keep)?
            && catch_up(&journal, window, PRUNE_STEP_ROWS, stop)? == CatchUp::Interrupted
        {
            return Ok(());
        }
    }
    compact_journal(path)
}

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

/// Prunes in steps of [`Journal::prune_step`] until caught up, each on the
/// blocking pool, so a cancel lands between steps. After each step it pauses
/// for as long as the step took, at least [`STEP_PAUSE`], so the stager's
/// writes, which ord's commits wait on, hold the journal at least half the
/// time.
async fn prune_once(journal: &Journal, modules: &[&'static str], keep: u32) -> Result<()> {
    loop {
        let Some(window) = window(journal, lowest_module_height(modules), keep)? else {
            return Ok(());
        };
        let worker = journal.clone();
        let started = Instant::now();
        let step = tokio::task::spawn_blocking(move || {
            worker.prune_step(window.raw, window.records, PRUNE_STEP_ROWS)
        })
        .await
        .context("pruning task panicked")??;
        let took = started.elapsed();
        if !step.pruned.is_empty() {
            tracing::debug!(
                raw_below = window.raw,
                records_below = window.records,
                pruned = ?step.pruned,
                "pruned"
            );
        }
        if step.done {
            return Ok(());
        }
        tokio::time::sleep(took.max(STEP_PAUSE)).await;
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

/// An enabled module's database, opened read-only.
pub(crate) struct ModuleDb {
    pub(crate) name: &'static str,
    options: PgConnectOptions,
}

impl ModuleDb {
    /// `name`'s database at `options`, with every transaction read-only.
    pub(crate) fn new(name: &'static str, options: PgConnectOptions) -> ModuleDb {
        ModuleDb {
            name,
            options: options.options([("default_transaction_read_only", "on")]),
        }
    }

    /// The module's table of stored block hashes. Module pruning keeps its
    /// highest row, the module's height.
    fn block_hashes_table(&self) -> String {
        format!("{}_block_hashes", self.name)
    }

    /// The module's height: the highest `block_height` of its block hashes
    /// table, `None` when it has stored no block.
    async fn height(&self) -> Result<Option<u32>> {
        let read = async {
            let mut connection = PgConnection::connect_with(&self.options).await?;
            let height: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT max(block_height)::int8 FROM {}",
                self.block_hashes_table()
            )))
            .fetch_one(&mut connection)
            .await?;
            connection.close().await.ok();
            Ok::<_, sqlx::Error>(height)
        };
        let height = tokio::time::timeout(HEIGHT_TIMEOUT, read)
            .await
            .map_err(|_| anyhow!("no answer within {HEIGHT_TIMEOUT:?}"))??;
        height
            .map(|height| u32::try_from(height).context("a negative height"))
            .transpose()
    }
}

/// The databases of the modules `configs` enables.
pub(crate) fn module_dbs(configs: &ModuleConfigs) -> Result<Vec<ModuleDb>> {
    let options = |host: &str, port: u16, user: &str, password: Option<&str>, db: &str, ssl| {
        let options = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(user)
            .database(db)
            .ssl_mode(if ssl {
                PgSslMode::Require
            } else {
                PgSslMode::Prefer
            });
        match password {
            Some(password) => options.password(password),
            None => options,
        }
    };
    let mut dbs = Vec::new();
    if let Some(c) = &configs.brc20 {
        let port = c
            .db_port
            .parse()
            .with_context(|| format!("DB_PORT must be a port number, got `{}`", c.db_port))?;
        let password = Some(c.db_password.as_str()).filter(|password| !password.is_empty());
        let db = options(
            &c.db_host,
            port,
            &c.db_user,
            password,
            &c.db_database,
            c.db_ssl,
        );
        dbs.push(ModuleDb::new("brc20", db));
    }
    if let Some(c) = &configs.bitmap {
        let password = c.db_password.as_deref();
        let db = options(
            &c.db_host,
            c.db_port,
            &c.db_user,
            password,
            &c.db_database,
            c.db_ssl,
        );
        dbs.push(ModuleDb::new("bitmap", db));
    }
    if let Some(c) = &configs.sns {
        let password = c.db_password.as_deref();
        let db = options(
            &c.db_host,
            c.db_port,
            &c.db_user,
            password,
            &c.db_database,
            c.db_ssl,
        );
        dbs.push(ModuleDb::new("sns", db));
    }
    if let Some(c) = &configs.runes {
        let password = c.db_password.as_deref();
        let db = options(
            &c.db_host,
            c.db_port,
            &c.db_user,
            password,
            &c.db_database,
            c.db_ssl,
        );
        dbs.push(ModuleDb::new("runes", db));
    }
    Ok(dbs)
}

/// The lowest height of `modules`, read from their databases. Fails, naming
/// the module and the reason, when a module's height cannot be read or it
/// has stored no block.
pub(crate) async fn lowest_height(modules: &[ModuleDb]) -> Result<u32> {
    let mut lowest = None::<u32>;
    for module in modules {
        let table = module.block_hashes_table();
        let height = module
            .height()
            .await
            .with_context(|| format!("reading {}'s height from {table}", module.name))?
            .with_context(|| {
                format!(
                    "{} has no height: {table} holds no block, so it needs the whole journal",
                    module.name
                )
            })?;
        lowest = Some(lowest.map_or(height, |lowest| lowest.min(height)));
    }
    lowest.context("no module is enabled")
}

/// [`lowest_height`] for `run`'s catch-up: `None`, after a warning, when it
/// fails, so the catch-up prunes raw events only and the background pruner
/// prunes the records once every module runs.
pub(crate) async fn lowest_height_or_warn(modules: &[ModuleDb]) -> Option<u32> {
    match lowest_height(modules).await {
        Ok(lowest) => Some(lowest),
        Err(err) => {
            tracing::warn!("the journal catch-up prunes raw events only: {err:#}");
            None
        }
    }
}

/// Rewrites the journal file at `path` without the pages pruning freed.
pub(crate) fn compact_journal(path: &Path) -> Result<()> {
    let before = std::fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    tracing::info!(path = %path.display(), bytes = before, "compacting the journal");
    Journal::compact(path)
        .map_err(|err| in_use(err, path))
        .with_context(|| format!("compacting {}", path.display()))?;
    let after = std::fs::metadata(path)?.len();
    tracing::info!(
        bytes = after,
        freed = before.saturating_sub(after),
        "compacted"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        opi_source::journal::{
            cf, table,
            testing::{self, Density},
        },
        redb::ReadableDatabase,
        std::{cell::Cell, path::PathBuf},
    };

    const DENSITY: Density = Density {
        raw_events: 40,
        creations: 10,
        transfers: 6,
        content: 32,
    };
    /// Heights 0..200 are final, so the tip is 199.
    const HEIGHTS: std::ops::Range<u32> = 0..200;
    const KEEP: u32 = 50;
    /// The lowest module height; records go below 71, raw events below 150.
    const LOWEST: u32 = 120;
    const RAW_BELOW: u32 = 150;
    const RECORDS_BELOW: u32 = 71;

    /// A journal holding every height of [`HEIGHTS`], none pruned.
    fn journal_in(dir: &Path) -> PathBuf {
        let path = dir.join("journal.redb");
        let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
        testing::fill(&journal, HEIGHTS, DENSITY).unwrap();
        path
    }

    fn rows_at(journal: &Journal, name: &'static str, height: u32) -> u64 {
        let tx = journal.db().begin_read().unwrap();
        let rows = tx.open_table(table(name)).unwrap();
        let (first, end) = (height.to_be_bytes(), (height + 1).to_be_bytes());
        rows.range::<&[u8]>(first.as_slice()..end.as_slice())
            .unwrap()
            .count() as u64
    }

    fn has_info(journal: &Journal, height: u32, index: u32) -> bool {
        let id = testing::inscription_id(height, index);
        let tx = journal.db().begin_read().unwrap();
        let infos = tx.open_table(table(cf::INFO)).unwrap();
        infos
            .get(opi_source::journal::info_key(&id).unwrap().as_slice())
            .unwrap()
            .is_some()
    }

    /// The rules of OPI_PRUNE on the journal at `path`, pruned with the
    /// lowest module height [`LOWEST`] and [`KEEP`] kept blocks: raw events
    /// go only below the final tip minus the window, records and consumed
    /// content only below the lowest module height minus the window, and
    /// everything a module still reads stays. Opening the journal also
    /// proves nothing else holds it.
    fn assert_pruned_to_window(path: &Path) {
        let journal = Journal::open(path, Chain::Regtest, false).unwrap();
        let records = u64::from(DENSITY.creations + DENSITY.transfers);
        let raw = u64::from(DENSITY.raw_events);

        assert_eq!(journal.raw_floor().unwrap(), Some(RAW_BELOW));
        assert_eq!(rows_at(&journal, cf::RAW_EVENTS, RAW_BELOW - 1), 0);
        for height in [RAW_BELOW, HEIGHTS.end - 1] {
            assert_eq!(rows_at(&journal, cf::RAW_EVENTS, height), raw);
            assert_eq!(rows_at(&journal, cf::RAW_HEIGHTS, height), 1);
        }

        assert_eq!(journal.pruned_below().unwrap(), RECORDS_BELOW);
        assert_eq!(rows_at(&journal, cf::TRANSFERS, RECORDS_BELOW - 1), 0);
        for height in [RECORDS_BELOW, LOWEST, HEIGHTS.end - 1] {
            assert_eq!(rows_at(&journal, cf::TRANSFERS, height), records);
        }
        // Content of an inscription first transferred below the floor goes;
        // content first transferred at the floor, or never, stays.
        assert!(!has_info(&journal, RECORDS_BELOW - 2, 0));
        assert!(has_info(&journal, RECORDS_BELOW - 1, 0));
        assert!(has_info(&journal, RECORDS_BELOW - 2, DENSITY.transfers));
        assert!(has_info(&journal, 0, DENSITY.creations - 1));
    }

    #[test]
    fn the_window_keeps_blocks_below_the_tip_and_the_lowest_module() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = Journal::open(&tmp.path().join("j.redb"), Chain::Regtest, false).unwrap();
        assert_eq!(window(&journal, Some(5), KEEP).unwrap(), None);
        testing::fill(&journal, HEIGHTS, DENSITY).unwrap();
        let at = |lowest| window(&journal, lowest, KEEP).unwrap().unwrap();
        assert_eq!(
            at(Some(LOWEST)),
            Window {
                raw: RAW_BELOW,
                records: RECORDS_BELOW
            }
        );
        // An unknown module height prunes no record.
        assert_eq!(at(None).records, 0);
        // A module above the journal's tip prunes no more than the tip allows.
        assert_eq!(at(Some(500)).records, RAW_BELOW);
        assert_eq!(at(Some(10)).records, 0);
    }

    /// `run` prunes a journal far behind the window before it starts the
    /// source and the modules: `start` runs only once the catch-up is done
    /// and has closed the journal, so no other writer runs during it.
    #[test]
    fn run_prunes_the_journal_before_it_starts_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let path = journal_in(tmp.path());
        let started = Cell::new(false);
        let out = catch_up_then(
            &path,
            Chain::Regtest,
            false,
            Some(LOWEST),
            KEEP,
            &|| false,
            || {
                assert_pruned_to_window(&path);
                started.set(true);
                Ok(7)
            },
        )
        .unwrap();
        assert_eq!(out, Some(7));
        assert!(started.get());
    }

    /// With no module height, `run`'s catch-up prunes raw events only.
    #[test]
    fn run_without_a_module_height_prunes_raw_events_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = journal_in(tmp.path());
        catch_up_then(&path, Chain::Regtest, false, None, KEEP, &|| false, || {
            let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
            assert_eq!(journal.raw_floor().unwrap(), Some(RAW_BELOW));
            assert_eq!(journal.pruned_below().unwrap(), 0);
            assert!(has_info(&journal, 0, 0));
            Ok(())
        })
        .unwrap();
    }

    /// A catch-up stopped between two steps leaves whole heights and runs as
    /// the journal_prune fill; the next start resumes it, and a stopped
    /// catch-up never starts the source.
    #[test]
    fn an_interrupted_catch_up_resumes_on_the_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let path = journal_in(tmp.path());
        {
            let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
            let window = window(&journal, Some(LOWEST), KEEP).unwrap().unwrap();
            let (checks, saw_fill) = (Cell::new(0), Cell::new(false));
            let stop = || {
                checks.set(checks.get() + 1);
                let fills = opi_core::metrics::global().running_fills();
                saw_fill.set(saw_fill.get() || fills.contains(&FILL));
                checks.get() > 5
            };
            assert_eq!(
                catch_up(&journal, window, 50, &stop).unwrap(),
                CatchUp::Interrupted
            );
            assert!(saw_fill.get());
            let (raw_left, records_left) = remaining(&journal, window).unwrap();
            assert!(
                raw_left > 0 && records_left > 0,
                "{raw_left} {records_left}"
            );
            let floor = journal.pruned_below().unwrap();
            assert!(floor > 0);
            assert_eq!(
                rows_at(&journal, cf::TRANSFERS, floor),
                u64::from(DENSITY.creations + DENSITY.transfers)
            );
            // A raw height pruned in part keeps its tag, so its rest goes
            // on the next start.
            let raw_floor = journal.raw_floor().unwrap().unwrap();
            assert_eq!(rows_at(&journal, cf::RAW_HEIGHTS, raw_floor), 1);
        }

        let stopped = catch_up_then(
            &path,
            Chain::Regtest,
            false,
            Some(LOWEST),
            KEEP,
            &|| true,
            || panic!("a stopped catch-up starts nothing"),
        );
        assert_eq!(stopped.unwrap(), None::<()>);

        catch_up_then(
            &path,
            Chain::Regtest,
            false,
            Some(LOWEST),
            KEEP,
            &|| false,
            || {
                assert_pruned_to_window(&path);
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
    }

    fn unreachable(name: &'static str) -> ModuleDb {
        ModuleDb::new(
            name,
            PgConnectOptions::new()
                .host("127.0.0.1")
                .port(1)
                .username("nobody")
                .database("nothing"),
        )
    }

    /// `compact-journal` with OPI_PRUNE refuses when a module's height
    /// cannot be read, naming the module, and leaves the journal as it was.
    #[test]
    fn compact_journal_refuses_without_every_module_height() {
        let tmp = tempfile::tempdir().unwrap();
        let path = journal_in(tmp.path());
        let before = std::fs::metadata(&path).unwrap().len();
        let modules = [unreachable("bitmap")];
        let prune = OfflinePrune {
            chain: Chain::Regtest,
            runes: false,
            keep: KEEP,
            modules: &modules,
        };
        let err = compact_command(&path, Some(prune), &|| false).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("reading bitmap's height from bitmap_block_hashes"),
            "{message}"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        let journal = Journal::open(&path, Chain::Regtest, false).unwrap();
        assert_eq!(journal.pruned_below().unwrap(), 0);
        assert_eq!(journal.raw_floor().unwrap(), Some(0));
    }

    /// `compact-journal` refuses while `run` holds the journal, with or
    /// without OPI_PRUNE.
    #[test]
    fn compact_journal_refuses_while_run_holds_the_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = journal_in(tmp.path());
        let _held = Journal::open(&path, Chain::Regtest, false).unwrap();
        let modules = [unreachable("brc20")];
        for prune in [
            None,
            Some(OfflinePrune {
                chain: Chain::Regtest,
                runes: false,
                keep: KEEP,
                modules: &modules,
            }),
        ] {
            let err = compact_command(&path, prune, &|| false).unwrap_err();
            assert!(
                format!("{err:#}").contains("is in use; stop `opi-indexer run` first"),
                "{err:#}"
            );
        }
    }

    fn admin() -> PgConnectOptions {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("alper")
            .database("postgres")
    }

    /// Creates a throwaway database named `name` whose `<module>_block_hashes`
    /// holds `heights`.
    async fn module_database(root: &mut PgConnection, name: &str, module: &str, heights: &[i32]) {
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut *root)
            .await
            .unwrap();
        let mut db = PgConnection::connect_with(&admin().database(name))
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE {module}_block_hashes (block_height int4 NOT NULL, block_hash text NOT NULL)"
        )))
        .execute(&mut db)
        .await
        .unwrap();
        for height in heights {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {module}_block_hashes VALUES ($1, 'hash')"
            )))
            .bind(height)
            .execute(&mut db)
            .await
            .unwrap();
        }
        db.close().await.unwrap();
    }

    /// `compact-journal` with OPI_PRUNE reads the module heights from their
    /// databases, read-only, prunes to the window below the lowest, then
    /// compacts; a module with no stored block refuses.
    #[test]
    fn compact_journal_prunes_from_the_module_databases_then_compacts() {
        if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
            eprintln!("OPI_TEST_PG is not 1; skipping");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let prefix = format!("opi_prune_test_{}_{nanos}", std::process::id());
        let names = ["brc20", "bitmap", "sns"].map(|module| format!("{prefix}_{module}"));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut root = runtime
            .block_on(PgConnection::connect_with(&admin()))
            .unwrap();
        let lowest = LOWEST as i32;
        runtime.block_on(async {
            module_database(&mut root, &names[0], "brc20", &[lowest + 10, lowest + 3]).await;
            module_database(&mut root, &names[1], "bitmap", &[lowest - 5, lowest]).await;
            module_database(&mut root, &names[2], "sns", &[]).await;
        });

        let result = std::panic::catch_unwind(|| {
            let db = |module, index: usize| ModuleDb::new(module, admin().database(&names[index]));
            let tmp = tempfile::tempdir().unwrap();
            let path = journal_in(tmp.path());

            let modules = [db("brc20", 0), db("bitmap", 1), db("sns", 2)];
            let prune = OfflinePrune {
                chain: Chain::Regtest,
                runes: false,
                keep: KEEP,
                modules: &modules,
            };
            let err = compact_command(&path, Some(prune), &|| false).unwrap_err();
            assert!(
                format!("{err:#}").contains("sns has no height: sns_block_hashes holds no block"),
                "{err:#}"
            );
            assert_eq!(
                Journal::open(&path, Chain::Regtest, false)
                    .unwrap()
                    .pruned_below()
                    .unwrap(),
                0
            );

            let before = std::fs::metadata(&path).unwrap().len();
            let modules = [db("brc20", 0), db("bitmap", 1)];
            let prune = OfflinePrune {
                chain: Chain::Regtest,
                runes: false,
                keep: KEEP,
                modules: &modules,
            };
            compact_command(&path, Some(prune), &|| false).unwrap();
            assert!(std::fs::metadata(&path).unwrap().len() < before);
            assert_pruned_to_window(&path);
        });

        runtime.block_on(async {
            for name in &names {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE {name} WITH (FORCE)"
                )))
                .execute(&mut root)
                .await
                .unwrap();
            }
        });
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    /// The module heights connection refuses writes.
    #[test]
    fn module_databases_open_read_only() {
        let db = unreachable("runes");
        let read_only = db
            .options
            .get_options()
            .is_some_and(|options| options.contains("default_transaction_read_only=on"));
        assert!(read_only, "{:?}", db.options.get_options());
    }
}
