use std::{
    collections::HashMap,
    error::Error,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use anyhow::anyhow;
use bitcoin::Network;
use brc20_prog::Brc20Prog;
use opi_core::{BlockRef, BlockSource, BlockView, InscriptionFeed, InscriptionInfo, Module};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::{
    config::Brc20IndexerConfig,
    database::{Brc20Database, get_brc20_database, replace_brc20_database},
    fail_stop::BRC20_THREAD_NAME,
    indexer::Brc20Indexer,
    types::Brc20Tx,
};

/// The BRC-20 and BRC2.0 indexer as an opi-indexer module. It stores its
/// state in brc20's Postgres database and drives brc20-prog.
///
/// The indexer and brc20's database live on one dedicated thread for the
/// life of the process (see `spawn_on_brc20_thread`). Each instance runs a
/// task there that serves this handle's requests in order and ends when the
/// handle is dropped. Only one such task runs at a time: a new instance waits
/// for the previous one to end before it installs its fresh database.
///
/// A cancelled `apply` drops only the wait for the reply; the in-flight block
/// runs to completion on the brc20 thread. That is safe because the next
/// instance's `init` clears any residue of a partly written block and
/// reconciles brc20 with brc20-prog before it indexes again.
pub struct Brc20Module {
    requests: mpsc::UnboundedSender<Request>,
    network: Network,
    first_inscription_height: i32,
    height_limit: i32,
}

type Reply<T> = oneshot::Sender<anyhow::Result<T>>;

enum Request {
    Cursor(Reply<Option<BlockRef>>),
    BlockHash(i32, Reply<Option<String>>),
    RollbackTo(i32, Reply<()>),
    IndexBlock {
        height: i32,
        hash: String,
        time: i64,
        transfers: Vec<Brc20Tx>,
        reply: Reply<()>,
    },
}

impl Brc20Module {
    /// Builds an instance on a fresh database in brc20's global slot and runs
    /// the indexer's start-up: schema, the Bitcoin RPC proxy when enabled,
    /// residue cleanup and the reconciliation of brc20 with brc20-prog, the
    /// trace hash upgrade, and brc20-prog's version check and initialisation.
    /// `journal` serves the tip and the block times that reporting reads.
    pub async fn new(
        config: Brc20IndexerConfig,
        brc20_prog: Brc20Prog,
        journal: Arc<dyn BlockSource>,
    ) -> anyhow::Result<Brc20Module> {
        if config.light_client_mode {
            return Err(anyhow!(
                "the brc20 module runs in full mode; set OPERATION_MODE=full"
            ));
        }
        let network = config.network_type;
        let first_inscription_height = config.first_inscription_height;
        let height_limit = config.height_limit;

        let (requests, receiver) = mpsc::unbounded_channel();
        let (ready, started) = oneshot::channel();
        spawn_on_brc20_thread(Box::new(move || {
            Box::pin(serve(config, brc20_prog, journal, receiver, ready))
        }));
        started.await.map_err(|_| worker_stopped())??;

        Ok(Brc20Module {
            requests,
            network,
            first_inscription_height,
            height_limit,
        })
    }

    async fn request<T>(&self, request: impl FnOnce(Reply<T>) -> Request) -> anyhow::Result<T> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(request(reply))
            .map_err(|_| worker_stopped())?;
        response.await.map_err(|_| worker_stopped())?
    }
}

#[async_trait::async_trait]
impl Module for Brc20Module {
    fn name(&self) -> &'static str {
        "brc20"
    }

    /// brc20 stores a block hash for every height from the first inscription
    /// height on.
    fn start_height(&self) -> u32 {
        self.first_inscription_height as u32
    }

    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>> {
        self.request(Request::Cursor).await
    }

    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>> {
        self.request(|reply| Request::BlockHash(height as i32, reply))
            .await
    }

    /// Reorgs brc20's database and brc20-prog to `height`.
    async fn rollback_to(&mut self, height: u32) -> anyhow::Result<()> {
        self.request(|reply| Request::RollbackTo(height as i32, reply))
            .await
    }

    /// Indexes the block, or idles from the configured height limit on.
    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()> {
        let height = block.height as i32;
        if height >= self.height_limit {
            tracing::info!(
                "Reached height limit of {}, stopping indexer.",
                self.height_limit
            );
            std::future::pending::<()>().await;
        }
        let transfers = transfers(&block.inscriptions, self.network)?;
        self.request(|reply| Request::IndexBlock {
            height,
            hash: block.hash.clone(),
            time: block.time as i64,
            transfers,
            reply,
        })
        .await
    }
}

/// Held by `serve` for an instance's whole life, so instances never share the
/// global database.
static INSTANCE: Mutex<()> = Mutex::const_new(());

/// Returns once no instance runs on the brc20 thread: the last module handle
/// is dropped and the request in flight, such as an engine commit, has
/// finished.
pub async fn wait_until_idle() {
    drop(INSTANCE.lock().await);
}

/// Starts the indexer and answers `requests` until the module handle is
/// dropped. `ready` receives the start-up's outcome.
async fn serve(
    config: Brc20IndexerConfig,
    brc20_prog: Brc20Prog,
    journal: Arc<dyn BlockSource>,
    mut requests: mpsc::UnboundedReceiver<Request>,
    ready: oneshot::Sender<anyhow::Result<()>>,
) {
    let _instance = INSTANCE.lock().await;

    install_fresh_database(&config).await;
    let mut indexer = Brc20Indexer::new(config, brc20_prog, Some(journal));
    let started = indexer.init().await.map_err(into_anyhow);
    let failed = started.is_err();
    if ready.send(started).is_err() || failed {
        return;
    }

    while let Some(request) = requests.recv().await {
        match request {
            Request::Cursor(reply) => {
                let _ = reply.send(cursor().await);
            }
            Request::BlockHash(height, reply) => {
                let database = get_brc20_database();
                let database = database.lock().await;
                let _ = reply.send(stored_hash(&database, height).await);
            }
            Request::RollbackTo(height, reply) => {
                let _ = reply.send(indexer.reorg(height).await.map_err(into_anyhow));
            }
            Request::IndexBlock {
                height,
                hash,
                time,
                transfers,
                reply,
            } => {
                let _ = reply.send(index_block(&mut indexer, height, &hash, time, transfers).await);
            }
        }
    }
}

/// Checks that brc20 and brc20-prog stand at the same height, refreshes the
/// journal tip when the block is past the last one read, and indexes the block.
async fn index_block(
    indexer: &mut Brc20Indexer,
    height: i32,
    hash: &str,
    time: i64,
    transfers: Vec<Brc20Tx>,
) -> anyhow::Result<()> {
    indexer
        .check_brc20_prog_height()
        .await
        .map_err(into_anyhow)?;
    // The tip read before indexing decides whether the block is the journal's
    // last one.
    if height > indexer.last_opi_block {
        indexer.last_opi_block = indexer.get_opi_block_height().await.map_err(into_anyhow)?;
    }
    indexer
        .index_block(height, hash, time, transfers)
        .await
        .map_err(into_anyhow)
}

/// brc20's highest stored block and its hash, `None` before the first block.
async fn cursor() -> anyhow::Result<Option<BlockRef>> {
    let database = get_brc20_database();
    let database = database.lock().await;
    let height: Option<i32> =
        sqlx::query_scalar("SELECT max(block_height) FROM brc20_block_hashes")
            .fetch_one(&database.client)
            .await?;
    let Some(height) = height else {
        return Ok(None);
    };
    let hash = stored_hash(&database, height)
        .await?
        .ok_or_else(|| anyhow!("brc20 has no block hash at its tip {height}"))?;
    Ok(Some(BlockRef {
        height: height as u32,
        hash,
    }))
}

async fn stored_hash(database: &Brc20Database, height: i32) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT block_hash FROM brc20_block_hashes WHERE block_height = $1")
            .bind(height)
            .fetch_optional(&database.client)
            .await?,
    )
}

fn into_anyhow(err: Box<dyn Error>) -> anyhow::Error {
    anyhow!("{err}")
}

fn worker_stopped() -> anyhow::Error {
    anyhow!("the brc20 task stopped, see the panic above")
}

type Job = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()>>> + Send>;

/// Runs `job`'s future on the brc20 thread.
///
/// The indexer's futures hold `Box<dyn Error>` values across awaits, so they
/// are not `Send` and run as local tasks on a current-thread runtime. One
/// thread serves every instance, so brc20's database pool always lives on the
/// runtime that created it.
fn spawn_on_brc20_thread(job: Job) {
    static JOBS: OnceLock<mpsc::UnboundedSender<Job>> = OnceLock::new();
    JOBS.get_or_init(|| {
        let (jobs, mut received) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name(BRC20_THREAD_NAME.into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building the brc20 runtime");
                tokio::task::LocalSet::new().block_on(&runtime, async move {
                    while let Some(job) = received.recv().await {
                        tokio::task::spawn_local(job());
                    }
                });
            })
            .expect("spawning the brc20 thread");
        jobs
    })
    .send(job)
    .unwrap_or_else(|_| panic!("the brc20 thread runs for the life of the process"));
}

/// Puts a newly constructed database into brc20's global slot, so the next
/// instance starts from what Postgres holds, with empty caches and no pending
/// writes.
pub async fn install_fresh_database(config: &Brc20IndexerConfig) {
    replace_brc20_database(Brc20Database::new(config)).await;
}

/// The BRC-20 transfers of a block's inscription feed, in record order.
/// Records of inscriptions that cannot carry a BRC-20, BRC2.0 program or
/// module operation are left out.
pub fn transfers(feed: &InscriptionFeed, network: Network) -> anyhow::Result<Vec<Brc20Tx>> {
    // Per inscription id: whether it is valid for BRC-20.
    let mut verdicts: HashMap<&str, bool> = HashMap::new();
    let mut txes = Vec::new();

    for record in &feed.records {
        let info = feed
            .infos
            .get(&record.inscription_id)
            .ok_or_else(|| anyhow!("Inscription info not found for {}", record.inscription_id))?;
        let valid = *verdicts
            .entry(record.inscription_id.as_str())
            .or_insert_with(|| is_valid_brc20(info));
        if !valid {
            continue;
        }

        let new_pkscript = hex::encode(&record.new_pkscript);
        txes.push(Brc20Tx {
            tx_id: format!("{}:{}", record.height, record.idx),
            inscription_id: record.inscription_id.clone(),
            inscription_number: info.inscription_number,
            old_satpoint: record.old_satpoint.clone(),
            new_satpoint: record.new_satpoint.clone(),
            txid: record.txid.clone(),
            new_wallet: get_wallet(&new_pkscript, network),
            new_pkscript,
            sent_as_fee: record.sent_as_fee,
            content: serde_json::from_slice(&info.content).unwrap_or(serde_json::Value::Null),
            byte_len: info.content.len() as u32,
            parent_id: info.parent_id.clone(),
        });
    }

    Ok(txes)
}

/// Whether an inscription can carry a BRC-20, BRC2.0 program or module
/// operation.
pub fn is_valid_brc20(inscription_info: &InscriptionInfo) -> bool {
    if inscription_info.cursed_for_brc20 {
        return false;
    }
    if !inscription_info.is_json {
        return false;
    }

    let Ok(decoded_content_type) = std::str::from_utf8(&inscription_info.content_type) else {
        return false;
    };

    if !decoded_content_type.eq("text/plain")
        && !decoded_content_type.starts_with("text/plain;")
        && !decoded_content_type.eq("application/json")
        && !decoded_content_type.starts_with("application/json;")
    {
        return false;
    }

    let json_data: serde_json::Value = match serde_json::from_slice(&inscription_info.content) {
        Ok(data) => data,
        Err(_) => return false, // Invalid JSON
    };

    let Some(protocol) = json_data.get("p").and_then(|v| v.as_str()) else {
        return false; // Missing or invalid 'p' field
    };
    if protocol != "brc-20" && protocol != "brc20-prog" && protocol != "brc20-module" {
        return false;
    }
    if protocol == "brc20-module" {
        let Some(module) = json_data.get("module").and_then(|m| m.as_str()) else {
            return false; // Missing or invalid 'module' field for brc20-module
        };
        if module != "BRC20PROG" {
            return false; // Invalid module value
        }
    }

    true
}

/// Address string for a hex pkscript, or an empty string when the script has
/// no address form.
pub fn get_wallet(pkscript: &str, network: Network) -> String {
    bitcoin::Address::from_script(
        bitcoin::Script::from_bytes(&hex::decode(pkscript).unwrap()),
        network,
    )
    .map(|addr| addr.to_string())
    .unwrap_or_else(|_| "".to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use super::*;

    /// A module whose instance holds `INSTANCE` as `serve` does and answers
    /// each request after blocking the brc20 thread, as an engine call does.
    /// Dropping the module mid-request leaves `wait_until_idle` pending until
    /// that request has finished.
    #[tokio::test]
    async fn wait_until_idle_waits_for_the_request_in_flight() {
        let (requests, mut receiver) = mpsc::unbounded_channel::<Request>();
        let (holding, held) = oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let answered = finished.clone();
        spawn_on_brc20_thread(Box::new(move || {
            Box::pin(async move {
                let _instance = INSTANCE.lock().await;
                holding.send(()).ok();
                while let Some(request) = receiver.recv().await {
                    std::thread::sleep(Duration::from_millis(500));
                    answered.store(true, Ordering::SeqCst);
                    if let Request::Cursor(reply) = request {
                        reply.send(Ok(None)).ok();
                    }
                }
            })
        }));
        held.await.unwrap();

        let mut module = Brc20Module {
            requests,
            network: Network::Regtest,
            first_inscription_height: 0,
            height_limit: i32::MAX,
        };
        // The caller gives up on the reply and drops the module, as a
        // shutdown does, while the request is still running.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), module.cursor())
                .await
                .is_err()
        );
        drop(module);
        assert!(!finished.load(Ordering::SeqCst));

        wait_until_idle().await;
        assert!(finished.load(Ordering::SeqCst));
    }
}
