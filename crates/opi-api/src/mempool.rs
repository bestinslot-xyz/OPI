//! The mempool view: bitcoind's unconfirmed transactions, held in memory,
//! for `/v1/utxos/wallet?mempool=true`.
//!
//! A poller reads `getrawmempool` every MEMPOOL_POLL_MS and fetches each new
//! transaction (`getrawtransaction`, `getmempoolentry`) in batches. A
//! transaction that leaves the mempool stays in the view until the utxos
//! module reaches the bitcoind tip it left at: by then the module holds the
//! block that confirmed it, so its outputs never vanish and its inputs never
//! reappear between the two. A transaction replaced without a block leaves
//! at a tip the module already holds and goes at the next poll. A module
//! catching up from far behind bridges nothing: a transaction goes once the
//! tip is [`state::MAX_LEFT_BLOCKS`] past where it left.

mod rpc;
pub mod state;

use {
    crate::{config::MempoolSettings, db::Db},
    anyhow::Result,
    bitcoin::{Transaction, Txid},
    state::{MempoolTx, State},
    std::{
        collections::HashSet,
        sync::{Arc, RwLock},
        time::{SystemTime, UNIX_EPOCH},
    },
    tokio::task::JoinHandle,
};

/// New transactions fetched per bitcoind batch.
const FETCH_BATCH: usize = 500;

/// What the view reads from bitcoind.
#[async_trait::async_trait]
pub trait Bitcoind: Send + Sync {
    async fn mempool_txids(&self) -> Result<Vec<Txid>>;
    async fn block_count(&self) -> Result<u32>;
    /// Each transaction with its fee in sats and vsize; `None` for one that
    /// left the mempool meanwhile.
    async fn entries(&self, txids: &[Txid]) -> Result<Vec<Option<(Transaction, u64, u64)>>>;
}

#[derive(Default)]
struct Inner {
    state: State,
    /// Unix seconds of the last complete poll; `None` before the first.
    updated_at: Option<u64>,
}

/// The view, shared by the poller and the routes.
#[derive(Clone, Default)]
pub struct Mempool {
    inner: Arc<RwLock<Inner>>,
}

impl std::fmt::Debug for Mempool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mempool")
            .field("updated_at", &self.updated_at())
            .finish_non_exhaustive()
    }
}

impl Mempool {
    /// Runs `read` on the current state.
    pub fn read<R>(&self, read: impl FnOnce(&State) -> R) -> R {
        read(
            &self
                .inner
                .read()
                .expect("the mempool lock is never poisoned")
                .state,
        )
    }

    /// Unix seconds of the last complete poll; `None` while the first one
    /// runs.
    pub fn updated_at(&self) -> Option<u64> {
        self.inner
            .read()
            .expect("the mempool lock is never poisoned")
            .updated_at
    }

    fn write<R>(&self, write: impl FnOnce(&mut Inner) -> R) -> R {
        write(
            &mut self
                .inner
                .write()
                .expect("the mempool lock is never poisoned"),
        )
    }

    /// One poll: adds the transactions new to bitcoind's mempool, marks the
    /// ones gone from it with the tip, and drops those the utxos module has
    /// reached (`utxos_height`, when it is known) or the tip has left behind.
    pub async fn poll(&self, bitcoind: &dyn Bitcoind, utxos_height: Option<u32>) -> Result<()> {
        let listed: HashSet<Txid> = bitcoind.mempool_txids().await?.into_iter().collect();
        // read after the listing, so a block that empties the mempool between
        // the two is counted in the tip
        let tip = bitcoind.block_count().await?;
        let live = self.read(State::live);
        let added: Vec<Txid> = listed.difference(&live).copied().collect();
        for chunk in added.chunks(FETCH_BATCH) {
            let entries = bitcoind.entries(chunk).await?;
            self.write(|inner| {
                for (tx, fee, vsize) in entries.into_iter().flatten() {
                    inner.state.insert(MempoolTx::new(&tx, fee, vsize));
                }
            });
        }
        self.write(|inner| {
            for txid in live.difference(&listed) {
                inner.state.leave(txid, tip);
            }
            inner.state.purge(utxos_height, tip);
            inner.updated_at = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs()),
            );
        });
        Ok(())
    }
}

/// Starts the poller for `settings`; it reads the utxos module's height from
/// `utxos` and runs until the handle is aborted.
pub fn spawn(settings: &MempoolSettings, utxos: Db) -> Result<(Mempool, JoinHandle<()>)> {
    let rpc = rpc::Rpc::new(settings)?;
    let mempool = Mempool::default();
    let view = mempool.clone();
    let every = settings.poll;
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut failing = false;
        loop {
            interval.tick().await;
            let height = match utxos_height(&utxos).await {
                Ok(height) => height,
                Err(err) => {
                    tracing::warn!("mempool view: reading the utxos height: {err:#}");
                    None
                }
            };
            match view.poll(&rpc, height).await {
                Ok(()) if failing => {
                    failing = false;
                    tracing::info!("mempool view: polling bitcoind again");
                }
                Ok(()) => {}
                Err(err) => {
                    if !failing {
                        tracing::warn!("mempool view: polling bitcoind failed: {err:#}");
                    }
                    failing = true;
                }
            }
        }
    });
    Ok((mempool, task))
}

async fn utxos_height(utxos: &Db) -> Result<Option<u32>> {
    let height: Option<i32> =
        sqlx::query_scalar("select max(block_height) from utxos_block_hashes")
            .fetch_one(utxos.pool()?)
            .await?;
    Ok(height.map(|height| height as u32))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{
            Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Witness, absolute::LockTime,
            hashes::Hash, transaction::Version,
        },
        std::sync::Mutex,
    };

    /// bitcoind: a mempool and a tip the test sets.
    #[derive(Default)]
    struct Fake {
        mempool: Mutex<Vec<Transaction>>,
        tip: Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl Bitcoind for Fake {
        async fn mempool_txids(&self) -> Result<Vec<Txid>> {
            Ok(self
                .mempool
                .lock()
                .unwrap()
                .iter()
                .map(Transaction::compute_txid)
                .collect())
        }
        async fn block_count(&self) -> Result<u32> {
            Ok(*self.tip.lock().unwrap())
        }
        async fn entries(&self, txids: &[Txid]) -> Result<Vec<Option<(Transaction, u64, u64)>>> {
            let mempool = self.mempool.lock().unwrap();
            Ok(txids
                .iter()
                .map(|txid| {
                    mempool
                        .iter()
                        .find(|tx| tx.compute_txid() == *txid)
                        .map(|tx| (tx.clone(), 100, 150))
                })
                .collect())
        }
    }

    fn tx(n: u8) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_byte_array([n; 32]),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        }
    }

    #[tokio::test]
    async fn a_poll_follows_the_mempool_through_a_block() {
        let fake = Fake::default();
        let view = Mempool::default();
        assert_eq!(view.updated_at(), None);
        *fake.tip.lock().unwrap() = 100;
        *fake.mempool.lock().unwrap() = vec![tx(1), tx(2)];
        view.poll(&fake, Some(100)).await.unwrap();
        assert_eq!(view.read(State::len), 2);
        assert!(view.updated_at().is_some());

        // Block 101 confirms tx(1); the utxos module is still at 100.
        *fake.tip.lock().unwrap() = 101;
        *fake.mempool.lock().unwrap() = vec![tx(2)];
        view.poll(&fake, Some(100)).await.unwrap();
        assert_eq!(view.read(State::len), 2, "tx(1) waits for the module");
        assert_eq!(view.read(State::live).len(), 1);

        view.poll(&fake, Some(101)).await.unwrap();
        assert_eq!(view.read(State::len), 1);
        let spent = OutPoint {
            txid: Txid::from_byte_array([2; 32]),
            vout: 0,
        };
        assert!(view.read(|state| state.is_spent(&spent)));
    }

    #[tokio::test]
    async fn a_replaced_transaction_goes_at_the_next_poll() {
        let fake = Fake::default();
        let view = Mempool::default();
        *fake.tip.lock().unwrap() = 100;
        *fake.mempool.lock().unwrap() = vec![tx(1)];
        view.poll(&fake, Some(100)).await.unwrap();
        *fake.mempool.lock().unwrap() = vec![tx(3)];
        view.poll(&fake, Some(100)).await.unwrap();
        let live = view.read(State::live);
        assert_eq!(live, HashSet::from([tx(3).compute_txid()]));
        assert_eq!(view.read(State::len), 1);
    }
}
