use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bitcoin::{
    Amount, Block, BlockHash, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
    Witness, absolute::LockTime, blockdata::constants::genesis_block, opcodes, script::Builder,
    transaction::Version,
};
use opi_core::{
    BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionRecord, MAX_ROLLBACK_DEPTH,
    Module, RuneFeed,
};
use opi_utxos::{BlockFetcher, UtxosConfig, UtxosModule, txid_bytes};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

/// A chain of blocks bitcoind serves, by height and by hash.
#[derive(Default)]
struct Chain {
    by_height: Vec<Block>,
    by_hash: HashMap<BlockHash, Block>,
}

impl Chain {
    /// Appends a block of `txdata` and returns its hash. Each block's nonce
    /// is its height, so equal transactions still make distinct blocks.
    fn push(&mut self, txdata: Vec<Transaction>) -> BlockHash {
        let mut header = genesis_block(Network::Regtest).header;
        header.nonce = self.by_height.len() as u32;
        if let Some(previous) = self.by_height.last() {
            header.prev_blockhash = previous.block_hash();
        }
        let block = Block { header, txdata };
        let hash = block.block_hash();
        self.by_hash.insert(hash, block.clone());
        self.by_height.push(block);
        hash
    }

    /// Drops every block above `height`, keeping them fetchable by hash.
    fn truncate(&mut self, height: u32) {
        self.by_height.truncate(height as usize + 1);
    }

    fn tip(&self) -> u32 {
        self.by_height.len() as u32 - 1
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Chain>>);

#[async_trait::async_trait]
impl BlockFetcher for Shared {
    async fn block_hash(&self, height: u32) -> anyhow::Result<BlockHash> {
        let chain = self.0.lock().unwrap();
        chain
            .by_height
            .get(height as usize)
            .map(Block::block_hash)
            .ok_or_else(|| anyhow::anyhow!("no block at {height}"))
    }

    async fn block(&self, hash: BlockHash) -> anyhow::Result<Block> {
        let chain = self.0.lock().unwrap();
        chain
            .by_hash
            .get(&hash)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no block {hash}"))
    }
}

/// The journal: the chain's tip, as the journal would report it.
impl BlockSource for Shared {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Ok(Some(self.0.lock().unwrap().tip()))
    }
    fn block_info(&self, _: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(None)
    }
    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }
    fn block_view(&self, _: u32) -> anyhow::Result<Option<BlockView>> {
        Ok(None)
    }
}

impl Shared {
    fn view(&self, height: u32) -> BlockView {
        let chain = self.0.lock().unwrap();
        BlockView {
            height,
            hash: chain.by_height[height as usize].block_hash().to_string(),
            time: 0,
            inscriptions: InscriptionFeed {
                records: Vec::new(),
                infos: Default::default(),
            },
            runes: RuneFeed::default(),
            inscription_moves: Vec::new(),
        }
    }
}

fn script(tag: u8) -> ScriptBuf {
    Builder::new()
        .push_opcode(opcodes::all::OP_PUSHNUM_1)
        .push_slice([tag; 32])
        .into_script()
}

fn tx(inputs: &[OutPoint], outputs: &[(u64, u8)], tag: u32) -> Transaction {
    let coinbase = inputs.is_empty();
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::from_consensus(tag),
        input: if coinbase {
            vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: Builder::new().push_int(tag as i64).into_script(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }]
        } else {
            inputs
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect()
        },
        output: outputs
            .iter()
            .map(|(value, tag)| TxOut {
                value: Amount::from_sat(*value),
                script_pubkey: script(*tag),
            })
            .collect(),
    }
}

fn out(tx: &Transaction, vout: u32) -> OutPoint {
    OutPoint {
        txid: tx.compute_txid(),
        vout,
    }
}

fn admin() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(5432)
        .username("alper")
        .database("postgres")
}

/// Runs `body` on a fresh database, dropped afterwards, when OPI_TEST_PG=1.
async fn with_database<F, Fut>(body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("opi_utxos_test_{}_{nanos}_{n}", std::process::id());
    let mut root = PgConnection::connect_with(&admin()).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut root)
        .await
        .unwrap();
    let result = tokio::spawn(body(name.clone())).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&mut root)
    .await
    .unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

fn config(name: &str) -> UtxosConfig {
    UtxosConfig {
        db_user: "alper".into(),
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_database: name.into(),
        db_password: None,
        db_ssl: false,
        network_type: "regtest".into(),
    }
}

async fn connect(name: &str) -> PgConnection {
    PgConnection::connect_with(
        &PgConnectOptions::new()
            .host("127.0.0.1")
            .username("alper")
            .database(name),
    )
    .await
    .unwrap()
}

/// The UTXO set as (txid, vout, value, height), sorted.
async fn utxos(conn: &mut PgConnection) -> Vec<(Vec<u8>, i32, i64, i32)> {
    sqlx::query("SELECT txid, vout, value, height FROM utxos ORDER BY txid, vout")
        .fetch_all(conn)
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}

fn row(tx: &Transaction, vout: u32, value: i64, height: i32) -> (Vec<u8>, i32, i64, i32) {
    (
        txid_bytes(&tx.compute_txid()).to_vec(),
        vout as i32,
        value,
        height,
    )
}

fn sorted(mut rows: Vec<(Vec<u8>, i32, i64, i32)>) -> Vec<(Vec<u8>, i32, i64, i32)> {
    rows.sort();
    rows
}

/// Pads the chain with empty coinbase-only blocks up to `height`.
fn pad_to(chain: &mut Chain, height: u32) {
    while chain.tip() < height {
        let tag = chain.by_height.len() as u32 + 10_000;
        chain.push(vec![tx(&[], &[], tag)]);
    }
}

#[tokio::test]
async fn catch_up_then_follow_then_roll_back() {
    with_database(|name| async move {
        let shared = Shared::default();
        let (a, b, c);
        {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![tx(&[], &[(50, 0)], 0)]); // genesis: never in the set
            a = tx(&[], &[(50, 1)], 1);
            chain.push(vec![a.clone()]);
            b = tx(&[out(&a, 0)], &[(30, 2), (20, 3)], 2);
            c = tx(&[out(&b, 1)], &[(19, 4)], 3);
            chain.push(vec![tx(&[], &[(50, 5)], 4), b.clone(), c.clone()]);
            pad_to(&mut chain, MAX_ROLLBACK_DEPTH + 5);
        }
        let mut module = UtxosModule::new(
            config(&name),
            Arc::new(shared.clone()),
            Arc::new(shared.clone()),
        )
        .await
        .unwrap();
        assert!(module.cursor().await.unwrap().is_none());

        // The catch-up writes up to MAX_ROLLBACK_DEPTH - 1 below the tip.
        let tip = MAX_ROLLBACK_DEPTH + 5;
        let target = tip - (MAX_ROLLBACK_DEPTH - 1);
        assert_eq!(module.catch_up().await.unwrap(), target + 1);
        assert_eq!(module.cursor().await.unwrap().unwrap().height, target);
        let mut conn = connect(&name).await;
        let set = utxos(&mut conn).await;
        // a:0 spent by b, b:1 spent by c in the same block; padding coinbases
        // have no outputs.
        let block2_coinbase = shared.0.lock().unwrap().by_height[2].txdata[0].clone();
        assert_eq!(
            set,
            sorted(vec![
                row(&b, 0, 30, 2),
                row(&c, 0, 19, 2),
                row(&block2_coinbase, 0, 50, 2),
            ])
        );

        // Following: apply the next blocks with undo data; one of them spends.
        let d;
        {
            let mut chain = shared.0.lock().unwrap();
            chain.truncate(tip);
            d = tx(&[out(&b, 0)], &[(29, 6)], 5);
        }
        for height in target + 1..=tip {
            module.apply(&shared.view(height)).await.unwrap();
        }
        let next = {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![tx(&[], &[(50, 7)], 6), d.clone()]);
            chain.tip()
        };
        module.apply(&shared.view(next)).await.unwrap();
        let set = utxos(&mut conn).await;
        assert!(set.contains(&row(&d, 0, 29, next as i32)));
        assert!(!set.contains(&row(&b, 0, 30, 2)));

        // Rolling back puts b:0 back and drops the block's outputs.
        module.rollback_to(next - 1).await.unwrap();
        assert_eq!(module.cursor().await.unwrap().unwrap().height, next - 1);
        assert_eq!(
            utxos(&mut conn).await,
            sorted(vec![
                row(&b, 0, 30, 2),
                row(&c, 0, 19, 2),
                row(&block2_coinbase, 0, 50, 2),
            ])
        );

        // Below the catch-up there is nothing to undo by.
        let err = module.rollback_to(target - 1).await.unwrap_err();
        assert!(err.to_string().contains("cannot roll back"), "{err}");
    })
    .await;
}

#[tokio::test]
async fn an_output_created_and_spent_above_the_rollback_stays_gone() {
    with_database(|name| async move {
        let shared = Shared::default();
        {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![tx(&[], &[], 0)]);
        }
        let mut module = UtxosModule::new(
            config(&name),
            Arc::new(shared.clone()),
            Arc::new(shared.clone()),
        )
        .await
        .unwrap();
        // The chain is shorter than the rollback window, so every block is
        // applied with undo data.
        module.apply(&shared.view(0)).await.unwrap();
        let a = tx(&[], &[(50, 1)], 1);
        let b = tx(&[out(&a, 0)], &[(49, 2)], 2);
        let height = {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![a.clone()]);
            chain.push(vec![tx(&[], &[], 3), b.clone()]);
            chain.tip()
        };
        module.apply(&shared.view(1)).await.unwrap();
        module.apply(&shared.view(height)).await.unwrap();
        let mut conn = connect(&name).await;
        assert_eq!(utxos(&mut conn).await, vec![row(&b, 0, 49, 2)]);

        // a was created and spent above height 0: nothing comes back.
        module.rollback_to(0).await.unwrap();
        assert!(utxos(&mut conn).await.is_empty());
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 0);

        // Applied again, one block back restores a.
        module.apply(&shared.view(1)).await.unwrap();
        module.apply(&shared.view(height)).await.unwrap();
        module.rollback_to(1).await.unwrap();
        assert_eq!(utxos(&mut conn).await, vec![row(&a, 0, 50, 1)]);
    })
    .await;
}

#[tokio::test]
async fn a_spend_of_an_unknown_output_fails_the_block() {
    with_database(|name| async move {
        let shared = Shared::default();
        let ghost = tx(&[], &[(1, 9)], 99);
        {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![tx(&[], &[], 0)]);
            chain.push(vec![tx(&[], &[], 1), tx(&[out(&ghost, 0)], &[(1, 1)], 2)]);
        }
        let mut module = UtxosModule::new(
            config(&name),
            Arc::new(shared.clone()),
            Arc::new(shared.clone()),
        )
        .await
        .unwrap();
        module.apply(&shared.view(0)).await.unwrap();
        let err = module.apply(&shared.view(1)).await.unwrap_err();
        assert!(err.to_string().contains("spend 1 outputs"), "{err}");
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 0);
    })
    .await;
}

#[tokio::test]
async fn pruning_drops_undo_data_and_raises_the_rollback_floor() {
    with_database(|name| async move {
        let shared = Shared::default();
        let a = tx(&[], &[(50, 1)], 1);
        {
            let mut chain = shared.0.lock().unwrap();
            chain.push(vec![tx(&[], &[], 0)]);
            chain.push(vec![a.clone()]);
            chain.push(vec![tx(&[], &[], 2), tx(&[out(&a, 0)], &[(49, 2)], 3)]);
            chain.push(vec![tx(&[], &[], 4)]);
        }
        let mut module = UtxosModule::new(
            config(&name),
            Arc::new(shared.clone()),
            Arc::new(shared.clone()),
        )
        .await
        .unwrap();
        for height in 0..=3 {
            module.apply(&shared.view(height)).await.unwrap();
        }
        module.prune_below(3).await.unwrap();
        let mut conn = connect(&name).await;
        let spent: i64 = sqlx::query_scalar("SELECT count(*) FROM utxos_spent")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(spent, 0);
        assert!(module.rollback_to(2).await.is_ok());
        assert!(module.rollback_to(1).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn a_second_network_is_refused() {
    with_database(|name| async move {
        let shared = Shared::default();
        UtxosModule::new(
            config(&name),
            Arc::new(shared.clone()),
            Arc::new(shared.clone()),
        )
        .await
        .unwrap();
        let mut other = config(&name);
        other.network_type = "signet".into();
        let err = UtxosModule::new(other, Arc::new(shared.clone()), Arc::new(shared.clone()))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("holds network regtest"), "{err}");
    })
    .await;
}
