//! Driver: runs ord in chunks behind the gate proxy, with the stager logging
//! its events, and finalizes every height ord commits.

use {
    crate::{
        check_ord_index, check_ord_settings,
        journal::{
            ChainData, EmissionOrderError, FINALIZE_BATCH, Journal, JournalReader, RuneModelError,
            finalize_range, init_path, remove_if_exists, reorg_check, sync_parent,
        },
        ord_settings,
        proxy::{GateProxy, ProxyConfig, UpstreamAuth},
        rpc::BitcoinRpc,
        stager::{Stager, StagerHandle, truncate_raw_above},
    },
    anyhow::{Context, Result, anyhow, bail, ensure},
    bitcoin::{Block, BlockHash},
    ord::{Chain, Index, settings::Settings},
    std::{
        path::{Path, PathBuf},
        sync::{Arc, Condvar, Mutex, OnceLock, Weak},
        thread,
        time::Duration,
    },
    tokio::{runtime::Runtime, sync::mpsc, task::JoinHandle},
};

/// Blocks ord indexes per `update()` while catching up.
pub const DEFAULT_CHUNK: u32 = 1000;

const JOURNAL_FILE: &str = "journal.redb";
const EVENT_BUFFER: usize = 10_000;
const BLOCK_CACHE: usize = 32;
const TIP_SLEEP: Duration = Duration::from_secs(2);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Heights below `final_tip` whose proxy hashes are kept.
const HASH_RETENTION: u32 = 100;

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub chain: Chain,
    pub ord_data_dir: PathBuf,
    pub journal_dir: PathBuf,
    pub bitcoin_rpc_url: String,
    pub bitcoin_auth: UpstreamAuth,
    /// Blocks per `update()`; [`DEFAULT_CHUNK`] unless testing.
    pub chunk: u32,
    /// Whether ord indexes runes and the journal keeps rune records. Fixed
    /// for the life of an ord data dir and journal.
    pub runes: bool,
}

impl SourceConfig {
    /// The journal file inside `journal_dir`.
    pub fn journal_path(&self) -> PathBuf {
        self.journal_dir.join(JOURNAL_FILE)
    }
}

/// A running source. Dropping it shuts it down and waits for the driver.
///
/// `join` and drop block on the source's own runtime, so use the handle from
/// a non-async context; async callers use `tokio::task::spawn_blocking`.
pub struct SourceHandle {
    journal: Journal,
    proxy: GateProxy,
    stop: Arc<Stop>,
    driver: Option<thread::JoinHandle<Result<()>>>,
    stager: Option<JoinHandle<Result<()>>>,
    runtime: Option<Runtime>,
}

impl SourceHandle {
    pub fn reader(&self) -> JournalReader {
        JournalReader::new(self.journal.clone())
    }

    /// Whether the driver thread has exited, by shutdown or by an error that
    /// [`SourceHandle::join`] reports.
    pub fn is_finished(&self) -> bool {
        self.driver
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    /// Closes the gate. ord indexes the blocks it already fetched and commits,
    /// the driver finalizes them and exits.
    pub fn shutdown(&self) {
        self.stop.stop(&self.proxy);
    }

    /// Waits for the driver to exit, then for the stager to sync and stop.
    /// Returns the first error either hit.
    pub fn join(mut self) -> Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        let driver = match self.driver.take() {
            Some(driver) => driver
                .join()
                .unwrap_or_else(|_| Err(anyhow!("driver thread panicked"))),
            None => Ok(()),
        };
        let stager = match (self.stager.take(), &self.runtime) {
            (Some(stager), Some(runtime)) => runtime
                .block_on(stager)
                .unwrap_or_else(|err| Err(anyhow!("stager task failed: {err}"))),
            _ => Ok(()),
        };
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
        match (driver, stager) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), Ok(())) | (Ok(()), Err(err)) => Err(err),
            (Err(driver), Err(stager)) => Err(stager.context(format!("{driver:#}"))),
        }
    }
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        if self.driver.is_some() {
            self.shutdown();
            self.finish().ok();
        }
    }
}

/// Starts the gate proxy, the stager and ord's index, checks the journal
/// against ord, and spawns the driver thread.
///
/// Fails when ord's settings break the commit barrier (see
/// [`check_ord_settings`]), when the journal belongs to another chain, when
/// the journal or ord's index was built with the other rune indexing setting
/// (see [`check_ord_index`]), or when ord's index holds committed blocks the
/// raw event log never logged (a new, older or foreign journal).
///
/// It blocks on the source's own runtime, so call it from a non-async
/// context; async callers use `tokio::task::spawn_blocking`.
pub fn spawn_source(cfg: SourceConfig) -> Result<SourceHandle> {
    ensure!(cfg.chunk > 0, "chunk must be positive");
    std::fs::create_dir_all(&cfg.journal_dir)
        .with_context(|| format!("creating {}", cfg.journal_dir.display()))?;
    let journal_path = cfg.journal_path();
    let journal = Journal::open(&journal_path, cfg.chain, cfg.runes).with_context(|| {
        format!(
            "opening the journal of ord's data dir {}",
            cfg.ord_data_dir.display()
        )
    })?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("opi-source")
        .build()
        .context("building source runtime")?;

    let (barriers, barriers_rx) = mpsc::channel(1);
    let proxy = runtime.block_on(GateProxy::start(
        ProxyConfig {
            upstream_url: cfg.bitcoin_rpc_url.clone(),
            auth: cfg.bitcoin_auth.clone(),
            block_cache: BLOCK_CACHE,
        },
        barriers,
    ))?;

    let (events, events_rx) = mpsc::channel(EVENT_BUFFER);
    let mut stager = Stager::new(journal.clone(), events_rx, barriers_rx, proxy.clone());
    let opened = Arc::new(OnceLock::<Weak<Index>>::new());
    stager.set_commit_guard(Arc::new({
        let opened = opened.clone();
        move || match opened.get().and_then(Weak::upgrade) {
            Some(index) => index.block_count(),
            None => Ok(0),
        }
    }));
    let stager_handle = stager.handle();
    let stager_task = runtime.spawn(stager.run());

    let started = (|| {
        let settings = ord_settings(cfg.chain, &cfg.ord_data_dir, &proxy.url(), None, cfg.runes)?;
        check_ord_settings(&settings, cfg.runes)?;
        init_ord_index(&cfg, &proxy.url(), settings.index())?;
        let index = Arc::new(Index::open_with_event_sender(&settings, Some(events))?);
        opened.set(Arc::downgrade(&index)).ok();
        check_ord_index(&index, &cfg.ord_data_dir, cfg.runes)?;
        check_startup(&journal, &index, &settings)?;
        Ok::<_, anyhow::Error>(index)
    })();

    let index = match started {
        Ok(index) => index,
        Err(err) => {
            // ord's index and its event sender are dropped, so the stager
            // stops.
            runtime.block_on(stager_task).ok();
            runtime.shutdown_background();
            if journal.created() {
                drop(journal);
                std::fs::remove_file(&journal_path).ok();
            }
            return Err(err);
        }
    };

    let stop = Arc::new(Stop::default());
    let driver = Driver {
        rpc: BitcoinRpc::new(cfg.bitcoin_rpc_url, cfg.bitcoin_auth)?,
        runtime: runtime.handle().clone(),
        index,
        journal: journal.clone(),
        proxy: proxy.clone(),
        stager: stager_handle,
        stop: stop.clone(),
        chunk: cfg.chunk,
    };
    let driver = thread::Builder::new()
        .name("opi-driver".into())
        .spawn(move || driver.run())
        .context("spawning driver thread")?;

    Ok(SourceHandle {
        journal,
        proxy,
        stop,
        driver: Some(driver),
        stager: Some(stager_task),
        runtime: Some(runtime),
    })
}

/// Makes sure ord's index file at `index` exists in a fully initialized state.
///
/// ord creates the file and its tables in separate steps, so a crash between
/// them leaves a table-less file. A missing index is therefore built at
/// `<index>.init` by an open that indexes no blocks, then renamed into place.
fn init_ord_index(cfg: &SourceConfig, rpc_url: &str, index: &Path) -> Result<()> {
    let init = init_path(index);
    remove_if_exists(&init)?;
    if index.exists() {
        return Ok(());
    }
    let settings = ord_settings(
        cfg.chain,
        &cfg.ord_data_dir,
        rpc_url,
        Some(&init),
        cfg.runes,
    )?;
    drop(Index::open_with_event_sender(&settings, None)?);
    std::fs::rename(&init, index)
        .with_context(|| format!("renaming {} into place", init.display()))?;
    sync_parent(index)
}

/// Refuses a journal whose raw event log can't hold every event of ord's
/// committed blocks, rolls the journal back to the highest block it shares
/// with ord, and drops raw events above ord's committed tip.
///
/// ord never commits above the journal's gate limit while the journal logs
/// its events, so ord's committed height above it means the journal is new,
/// an older copy, or another index's. A journal with no gate limit accepts
/// only an ord index below the first inscription height.
fn check_startup(journal: &Journal, index: &Index, settings: &Settings) -> Result<()> {
    let count = index.block_count()?;
    if journal.created() && count > settings.first_inscription_height() {
        bail!(
            "ord's index holds {count} committed blocks, but the raw event log is new and lacks \
             their events; delete the ord data dir or restore the journal"
        );
    }
    let ahead = match journal.gate_limit()? {
        Some(limit) => count
            .checked_sub(1)
            .is_some_and(|committed| committed > limit),
        None => count > settings.first_inscription_height(),
    };
    if ahead {
        bail!(
            "ord's index holds {count} committed blocks, above what this raw event log has \
             logged (gate limit {:?}): the journal is stale or belongs to another index; \
             delete the ord data dir or restore the matching journal",
            journal.gate_limit()?
        );
    }
    reorg_check(journal, index)?;
    truncate_raw_above(journal, count.checked_sub(1))
}

/// Shutdown flag. The gate only opens while the flag is clear, so a shutdown
/// can't be undone by a chunk the driver is about to start.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    fn stop(&self, proxy: &GateProxy) {
        let mut stopped = self.stopped.lock().unwrap();
        *stopped = true;
        proxy.close();
        self.wake.notify_all();
    }

    /// Opens the gate up to `chunk_end` unless stopped, after durably raising
    /// the journal's gate limit to it. Returns whether it opened.
    fn open(&self, journal: &Journal, proxy: &GateProxy, chunk_end: u32) -> Result<bool> {
        let stopped = self.stopped.lock().unwrap();
        if *stopped {
            return Ok(false);
        }
        // The limit only rises, so a bitcoind tip that falls below ord's
        // committed height can't put ord above it.
        if journal.gate_limit()?.is_none_or(|limit| limit < chunk_end) {
            journal.set_gate_limit(chunk_end)?;
        }
        proxy.set_chunk_end(chunk_end);
        Ok(true)
    }

    fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap()
    }

    /// Sleeps for `timeout` or until stopped. Returns whether stopped.
    fn sleep(&self, timeout: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap();
        let (stopped, _) = self
            .wake
            .wait_timeout_while(stopped, timeout, |stopped| !*stopped)
            .unwrap();
        *stopped
    }
}

struct Driver {
    rpc: BitcoinRpc,
    runtime: tokio::runtime::Handle,
    index: Arc<Index>,
    journal: Journal,
    proxy: GateProxy,
    stager: StagerHandle,
    stop: Arc<Stop>,
    chunk: u32,
}

impl Driver {
    /// Runs on its own thread: ord's updater sends events with
    /// `blocking_send`, which panics on a tokio worker. Each bitcoind tip it
    /// reads becomes the process metrics' chain tip.
    fn run(self) -> Result<()> {
        let mut backoff = MIN_BACKOFF;
        loop {
            let next = self.index.block_count()?;
            let tip = match self.runtime.block_on(self.rpc.block_count()) {
                Ok(tip) => {
                    opi_core::metrics::global().set_chain_tip(tip);
                    tip
                }
                Err(err) => {
                    log::warn!("reading bitcoind tip failed, retrying in {backoff:?}: {err:#}");
                    if self.stop.sleep(backoff) {
                        return Ok(());
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };

            let chunk_end = tip.min(next.saturating_add(self.chunk - 1));
            if !self.stop.open(&self.journal, &self.proxy, chunk_end)? {
                return Ok(());
            }
            if let Err(err) = self.index.update() {
                log::warn!("ord update failed, retrying in {backoff:?}: {err:#}");
                self.runtime.block_on(self.stager.discard_pending())?;
                if self.stop.sleep(backoff) {
                    return Ok(());
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
            self.runtime.block_on(self.stager.drain_now())?;
            if let Err(err) =
                reorg_check(&self.journal, &self.index).and_then(|_| self.finalize_committed())
            {
                if is_fatal(&err) {
                    return Err(err);
                }
                log::warn!("finalize failed, retrying in {backoff:?}: {err:#}");
                if self.stop.sleep(backoff) {
                    return Ok(());
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
            backoff = MIN_BACKOFF;

            if self.stop.is_stopped() {
                return Ok(());
            }
            let at_tip = self.index.block_count()? > tip;
            if at_tip && self.stop.sleep(TIP_SLEEP) {
                return Ok(());
            }
        }
    }

    /// Finalizes `final_tip + 1 ..=` ord's committed tip.
    fn finalize_committed(&self) -> Result<()> {
        let Some(committed) = self.index.block_count()?.checked_sub(1) else {
            return Ok(());
        };
        let mut first = self.journal.final_tip()?.map_or(0, |tip| tip + 1);
        while first <= committed {
            let last = committed.min(first + (FINALIZE_BATCH - 1));
            finalize_range(&self.journal, &self.index, first..=last, &mut &*self)?;
            first = last + 1;
        }
        if let Some(tip) = self.journal.final_tip()? {
            self.proxy.forget_below(tip.saturating_sub(HASH_RETENTION));
        }
        Ok(())
    }
}

/// Whether a finalize error means ord's events break the model finalize
/// relies on, which no retry can fix.
fn is_fatal(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.is::<EmissionOrderError>() || cause.is::<RuneModelError>())
}

/// Blocks come from the proxy cache, else from bitcoind directly, as a header
/// alone when finalize needs no transactions.
impl ChainData for &Driver {
    fn block(&mut self, hash: BlockHash, transactions: bool) -> Result<Block> {
        if let Some(block) = self.proxy.cached_block(&hash) {
            return Ok(block);
        }
        if transactions {
            return self.runtime.block_on(self.rpc.block(hash));
        }
        let header = self.runtime.block_on(self.rpc.header(hash))?;
        Ok(Block {
            header,
            txdata: Vec::new(),
        })
    }

    fn input_values(&mut self, hash: BlockHash) -> Result<Vec<Vec<u64>>> {
        self.runtime.block_on(self.rpc.input_values(hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rune_model_errors_are_fatal_through_context() {
        let err =
            anyhow::Error::new(RuneModelError("unbalanced".into())).context("finalizing height 7");
        assert!(is_fatal(&err));
    }

    #[test]
    fn io_errors_are_retried() {
        let err = anyhow!("getblock failed").context("finalizing height 7");
        assert!(!is_fatal(&err));
    }
}
