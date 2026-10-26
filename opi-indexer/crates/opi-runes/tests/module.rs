use std::future::Future;
use std::sync::Arc;

use opi_core::{
    BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord, Module,
    RuneCounter, RuneEtching, RuneEvent, RuneEventKind, RuneFeed, RuneTerms,
};
use opi_runes::{
    RunesConfig, RunesModule, block_event_hash, cumulative_hash, event_str, wallet_addr,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

/// A journal whose tip is 1000, or whose tip read fails.
struct Journal {
    tip_fails: bool,
}

impl BlockSource for Journal {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        if self.tip_fails {
            anyhow::bail!("journal tip unavailable");
        }
        Ok(Some(1000))
    }
    fn block_info(&self, _: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(None)
    }
    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }
    fn inscription_info(&self, _: &str) -> anyhow::Result<Option<InscriptionInfo>> {
        Ok(None)
    }
    fn block_view(&self, _: u32) -> anyhow::Result<Option<BlockView>> {
        Ok(None)
    }
}

fn journal() -> Arc<dyn BlockSource> {
    Arc::new(Journal { tip_fails: false })
}

fn admin() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(5432)
        .username("alper")
        .database("postgres")
}

fn config(database: &str, network: &str) -> RunesConfig {
    RunesConfig {
        db_user: "alper".into(),
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_database: database.into(),
        db_password: None,
        db_ssl: false,
        network_type: network.into(),
        report_to_indexer: false,
        report_url: String::new(),
        report_retries: 1,
        report_name: "test".into(),
    }
}

async fn connect(database: &str) -> PgConnection {
    PgConnection::connect_with(
        &PgConnectOptions::new()
            .host("127.0.0.1")
            .username("alper")
            .database(database),
    )
    .await
    .unwrap()
}

/// Runs `body` against a fresh database that is dropped afterwards, on
/// failure too.
async fn with_database<F, Fut>(prefix: &str, body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("opi_runes_{prefix}_{}_{nanos}", std::process::id());
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

fn txid(byte: &str) -> String {
    byte.repeat(32)
}

fn outpoint(byte: &str, vout: u32) -> String {
    format!("{}:{vout}", txid(byte))
}

fn pk1() -> Vec<u8> {
    hex::decode(format!("0014{}", "11".repeat(20))).unwrap()
}

fn pk2() -> Vec<u8> {
    hex::decode(format!("5120{}", "22".repeat(32))).unwrap()
}

fn plain(kind: RuneEventKind, tx: &str, rune_id: &str, amount: u128) -> RuneEvent {
    RuneEvent {
        kind,
        txid: txid(tx),
        outpoint: None,
        pkscript: None,
        rune_id: rune_id.into(),
        amount,
    }
}

fn located(
    kind: RuneEventKind,
    tx: &str,
    outpoint: String,
    pkscript: Vec<u8>,
    rune_id: &str,
    amount: u128,
) -> RuneEvent {
    RuneEvent {
        kind,
        txid: txid(tx),
        outpoint: Some(outpoint),
        pkscript: Some(pkscript),
        rune_id: rune_id.into(),
        amount,
    }
}

fn etching(rune_id: &str, tx: &str, rune: &str, premine: u128) -> RuneEtching {
    let block = rune_id.split(':').next().unwrap().parse().unwrap();
    RuneEtching {
        rune_id: rune_id.into(),
        block,
        divisibility: 2,
        etching: txid(tx),
        terms: None,
        number: 0,
        premine,
        rune: rune.into(),
        spacers: 0,
        symbol: None,
        timestamp: 1_700_000_000,
        turbo: false,
    }
}

fn counter(rune_id: &str, burned: u128, mints: u128) -> RuneCounter {
    RuneCounter {
        rune_id: rune_id.into(),
        burned,
        mints,
    }
}

fn view(height: u32, runes: RuneFeed) -> BlockView {
    BlockView {
        height,
        hash: format!("hash{height}"),
        time: 1_700_000_000 + height,
        inscriptions: InscriptionFeed {
            records: Vec::new(),
            infos: Default::default(),
        },
        runes,
    }
}

const JS_MAX_SAFE: i64 = 9_007_199_254_740_991;

/// Height 1 etches `1:1` with a premine to aa:0. Height 2 mints `1:1` to bb:0
/// and spends aa:0 into cc:0, burning 400. Height 3 spends bb:0 and cc:0 into
/// dd:0 while etching `3:1` there, mints `3:1` to ee:0 in the block that
/// etched it, and mints `1:1` to ff:0.
fn blocks() -> Vec<BlockView> {
    use RuneEventKind::*;
    let mut terms_rune = etching("3:1", "d0", "SECONDRUNE", 5);
    terms_rune.symbol = Some('$');
    terms_rune.number = 1;
    terms_rune.spacers = 4;
    terms_rune.turbo = true;
    terms_rune.terms = Some(RuneTerms {
        amount: Some(7),
        cap: Some(u128::MAX),
        height: (Some(10), Some(1 << 53)),
        offset: (None, Some(u64::MAX)),
    });
    vec![
        view(
            1,
            RuneFeed {
                events: vec![
                    plain(NewAllocation, "a0", "1:1", 1000),
                    located(Output, "a0", outpoint("a0", 0), pk1(), "1:1", 1000),
                ],
                etchings: vec![etching("1:1", "a0", "FIRSTRUNE", 1000)],
                counters: Vec::new(),
            },
        ),
        view(
            2,
            RuneFeed {
                events: vec![
                    plain(Mint, "b0", "1:1", 50),
                    located(Output, "b0", outpoint("b0", 0), pk2(), "1:1", 50),
                    located(Input, "c0", outpoint("a0", 0), pk1(), "1:1", 1000),
                    located(Output, "c0", outpoint("c0", 0), pk1(), "1:1", 600),
                    plain(Burn, "c0", "1:1", 400),
                ],
                etchings: Vec::new(),
                counters: vec![counter("1:1", 400, 1)],
            },
        ),
        view(
            3,
            RuneFeed {
                events: vec![
                    located(Input, "d0", outpoint("b0", 0), pk2(), "1:1", 50),
                    located(Input, "d0", outpoint("c0", 0), pk1(), "1:1", 600),
                    plain(NewAllocation, "d0", "3:1", 5),
                    located(Output, "d0", outpoint("d0", 0), pk2(), "1:1", 650),
                    located(Output, "d0", outpoint("d0", 0), pk2(), "3:1", 5),
                    plain(Mint, "e0", "3:1", 7),
                    located(Output, "e0", outpoint("e0", 0), pk1(), "3:1", 7),
                    plain(Mint, "f0", "1:1", 50),
                    located(Output, "f0", outpoint("f0", 1), pk1(), "1:1", 50),
                ],
                etchings: vec![terms_rune],
                counters: vec![counter("1:1", 400, 2), counter("3:1", 0, 1)],
            },
        ),
    ]
}

type OutpointRow = (
    String,
    String,
    Option<String>,
    Vec<String>,
    Vec<String>,
    i32,
    bool,
    Option<i32>,
);

async fn outpoint_rows(conn: &mut PgConnection) -> Vec<OutpointRow> {
    sqlx::query(
        "SELECT outpoint, pkscript, wallet_addr, rune_ids, balances::text[], block_height, \
         spent, spent_block_height FROM runes_outpoint_to_balances ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.get(0),
            r.get(1),
            r.get(2),
            r.get(3),
            r.get(4),
            r.get(5),
            r.get(6),
            r.get(7),
        )
    })
    .collect()
}

/// `(rune_id, burned, mints, last_updated_block_height)` of every entry.
async fn entry_totals(conn: &mut PgConnection) -> Vec<(String, String, String, i32)> {
    sqlx::query(
        "SELECT rune_id, burned::text, mints::text, last_updated_block_height \
         FROM runes_id_to_entry ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
    .collect()
}

fn s(value: &str) -> String {
    value.to_string()
}

#[tokio::test]
async fn module_round_trip() {
    with_database("round_trip", |name| async move {
        let mut module = RunesModule::new(config(&name, "regtest"), journal())
            .await
            .unwrap();
        assert_eq!(module.name(), "runes");
        assert_eq!(module.start_height(), 0);
        assert!(module.cursor().await.unwrap().is_none());
        for block in blocks() {
            module.apply(&block).await.unwrap();
        }
        let cursor = module.cursor().await.unwrap().unwrap();
        assert_eq!((cursor.height, cursor.hash.as_str()), (3, "hash3"));
        assert_eq!(
            module.block_hash(2).await.unwrap().as_deref(),
            Some("hash2")
        );

        let mut conn = connect(&name).await;
        let w1 = wallet_addr(&pk1(), "regtest");
        let w2 = wallet_addr(&pk2(), "regtest");
        assert!(w1.as_deref().unwrap().starts_with("bcrt1q"));
        assert!(w2.as_deref().unwrap().starts_with("bcrt1p"));
        let h1 = hex::encode(pk1());
        let h2 = hex::encode(pk2());
        assert_eq!(
            outpoint_rows(&mut conn).await,
            vec![
                (
                    outpoint("a0", 0),
                    h1.clone(),
                    w1.clone(),
                    vec![s("1:1")],
                    vec![s("1000")],
                    1,
                    true,
                    Some(2)
                ),
                (
                    outpoint("b0", 0),
                    h2.clone(),
                    w2.clone(),
                    vec![s("1:1")],
                    vec![s("50")],
                    2,
                    true,
                    Some(3)
                ),
                (
                    outpoint("c0", 0),
                    h1.clone(),
                    w1.clone(),
                    vec![s("1:1")],
                    vec![s("600")],
                    2,
                    true,
                    Some(3)
                ),
                (
                    outpoint("d0", 0),
                    h2.clone(),
                    w2.clone(),
                    vec![s("1:1"), s("3:1")],
                    vec![s("650"), s("5")],
                    3,
                    false,
                    None
                ),
                (
                    outpoint("e0", 0),
                    h1.clone(),
                    w1.clone(),
                    vec![s("3:1")],
                    vec![s("7")],
                    3,
                    false,
                    None
                ),
                (
                    outpoint("f0", 1),
                    h1.clone(),
                    w1.clone(),
                    vec![s("1:1")],
                    vec![s("50")],
                    3,
                    false,
                    None
                ),
            ]
        );

        // Events keep feed order with ascending ids across blocks.
        let events = sqlx::query(
            "SELECT id, event_type, txid, outpoint, pkscript, wallet_addr, rune_id, \
             amount::text, block_height FROM runes_events ORDER BY id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let ids: Vec<i64> = events.iter().map(|r| r.get(0)).collect();
        assert_eq!(ids, (1..=16).collect::<Vec<i64>>());
        let all: Vec<RuneEvent> = blocks().into_iter().flat_map(|b| b.runes.events).collect();
        for (row, event) in events.iter().zip(&all) {
            assert_eq!(row.get::<i32, _>(1), i32::from(event.kind.id()));
            assert_eq!(row.get::<String, _>(2), event.txid);
            assert_eq!(row.get::<Option<String>, _>(3), event.outpoint);
            assert_eq!(
                row.get::<Option<String>, _>(4),
                event.pkscript.as_ref().map(hex::encode)
            );
            assert_eq!(
                row.get::<Option<String>, _>(5),
                event
                    .pkscript
                    .as_ref()
                    .and_then(|pk| wallet_addr(pk, "regtest"))
            );
            assert_eq!(row.get::<String, _>(6), event.rune_id);
            assert_eq!(row.get::<String, _>(7), event.amount.to_string());
        }
        let heights: Vec<i32> = events.iter().map(|r| r.get(8)).collect();
        assert_eq!(
            heights,
            vec![1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3]
        );

        // Entries: the totals of 3:1 reach the entry etched in the same block.
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![
                (s("1:1"), s("400"), s("2"), 3),
                (s("3:1"), s("0"), s("1"), 3),
            ]
        );
        let entry = sqlx::query(
            "SELECT rune_block, divisibility, etching, terms_amount::text, terms_cap::text, \
             terms_height_l, terms_height_h, terms_offset_l, terms_offset_h, \"number\"::text, \
             premine::text, rune_name, spacers, symbol, extract(epoch from \"timestamp\")::int8, \
             turbo, genesis_height FROM runes_id_to_entry WHERE rune_id = '3:1'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(entry.get::<i32, _>(0), 3);
        assert_eq!(entry.get::<i32, _>(1), 2);
        assert_eq!(entry.get::<String, _>(2), txid("d0"));
        assert_eq!(entry.get::<Option<String>, _>(3).as_deref(), Some("7"));
        assert_eq!(
            entry.get::<Option<String>, _>(4),
            Some(u128::MAX.to_string())
        );
        assert_eq!(entry.get::<Option<i64>, _>(5), Some(10));
        assert_eq!(entry.get::<Option<i64>, _>(6), Some(JS_MAX_SAFE));
        assert_eq!(entry.get::<Option<i64>, _>(7), None);
        assert_eq!(entry.get::<Option<i64>, _>(8), Some(JS_MAX_SAFE));
        assert_eq!(entry.get::<String, _>(9), "1");
        assert_eq!(entry.get::<String, _>(10), "5");
        assert_eq!(entry.get::<String, _>(11), "SECONDRUNE");
        assert_eq!(entry.get::<i64, _>(12), 4);
        assert_eq!(
            entry.get::<Option<String>, _>(13).as_deref(),
            Some("24000000")
        );
        assert_eq!(entry.get::<i64, _>(14), 1_700_000_000);
        assert!(entry.get::<bool, _>(15));
        assert_eq!(entry.get::<i32, _>(16), 3);
        let first = sqlx::query(
            "SELECT symbol, terms_amount, terms_height_l FROM runes_id_to_entry \
             WHERE rune_id = '1:1'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(first.get::<Option<String>, _>(0), None);
        assert_eq!(first.get::<Option<String>, _>(1), None);
        assert_eq!(first.get::<Option<i64>, _>(2), None);

        let changes: Vec<(String, String, String, i32)> = sqlx::query(
            "SELECT rune_id, burned::text, mints::text, block_height \
             FROM runes_id_to_entry_changes ORDER BY id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
        assert_eq!(
            changes,
            vec![
                (s("1:1"), s("400"), s("1"), 2),
                (s("1:1"), s("400"), s("2"), 3),
                (s("3:1"), s("0"), s("1"), 3),
            ]
        );

        // Block hashes with their times, and the event hash chain.
        let times: Vec<(i32, String, i64)> = sqlx::query(
            "SELECT block_height, block_hash, extract(epoch from block_time)::int8 \
             FROM runes_block_hashes ORDER BY block_height",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
        assert_eq!(
            times,
            vec![
                (1, s("hash1"), 1_700_000_001),
                (2, s("hash2"), 1_700_000_002),
                (3, s("hash3"), 1_700_000_003),
            ]
        );
        let hashes: Vec<(i32, String, String)> = sqlx::query(
            "SELECT block_height, block_event_hash, cumulative_event_hash \
             FROM runes_cumulative_event_hashes ORDER BY block_height",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
        let mut previous: Option<String> = None;
        let mut expected = Vec::new();
        for block in blocks() {
            let strs: Vec<String> = block.runes.events.iter().map(event_str).collect();
            let block_hash = block_event_hash(&strs);
            let cumulative = cumulative_hash(previous.as_deref(), &block_hash);
            expected.push((block.height as i32, block_hash, cumulative.clone()));
            previous = Some(cumulative);
        }
        assert_eq!(hashes, expected);

        // A tip error at report time skips the report; the block stays
        // committed.
        let reporting = RunesConfig {
            report_to_indexer: true,
            report_url: "http://127.0.0.1:9/report".into(),
            ..config(&name, "regtest")
        };
        let mut reporting = RunesModule::new(reporting, Arc::new(Journal { tip_fails: true }))
            .await
            .unwrap();
        reporting
            .apply(&view(4, RuneFeed::default()))
            .await
            .unwrap();
        assert_eq!(reporting.cursor().await.unwrap().unwrap().height, 4);
    })
    .await;
}

#[tokio::test]
async fn rollback_restores_entries_and_unspends() {
    with_database("rollback", |name| async move {
        let mut module = RunesModule::new(config(&name, "regtest"), journal())
            .await
            .unwrap();
        for block in blocks() {
            module.apply(&block).await.unwrap();
        }
        let mut conn = connect(&name).await;

        module.rollback_to(2).await.unwrap();
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 2);
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![(s("1:1"), s("400"), s("1"), 2)]
        );
        let rows = outpoint_rows(&mut conn).await;
        let state: Vec<(String, bool, Option<i32>)> =
            rows.iter().map(|r| (r.0.clone(), r.6, r.7)).collect();
        assert_eq!(
            state,
            vec![
                (outpoint("a0", 0), true, Some(2)),
                (outpoint("b0", 0), false, None),
                (outpoint("c0", 0), false, None),
            ]
        );
        for (table, expected) in [
            ("runes_events", 7),
            ("runes_id_to_entry_changes", 1),
            ("runes_cumulative_event_hashes", 2),
            ("runes_block_hashes", 2),
        ] {
            let count: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(&mut conn)
                    .await
                    .unwrap();
            assert_eq!(count, expected, "{table}");
        }

        module.rollback_to(1).await.unwrap();
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![(s("1:1"), s("0"), s("0"), 1)]
        );
        let rows = outpoint_rows(&mut conn).await;
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].6, rows[0].7), (false, None));

        // Replaying the removed heights rebuilds the same rows and ids.
        for block in blocks().into_iter().skip(1) {
            module.apply(&block).await.unwrap();
        }
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM runes_events ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        assert_eq!(ids, (1..=16).collect::<Vec<i64>>());
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM runes_outpoint_to_balances ORDER BY id")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(ids, (1..=6).collect::<Vec<i64>>());
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![
                (s("1:1"), s("400"), s("2"), 3),
                (s("3:1"), s("0"), s("1"), 3),
            ]
        );

        // Rows above the cursor are residue that a new instance drops.
        sqlx::query("DELETE FROM runes_block_hashes WHERE block_height = 3")
            .execute(&mut conn)
            .await
            .unwrap();
        drop(module);
        let mut module = RunesModule::new(config(&name, "regtest"), journal())
            .await
            .unwrap();
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 2);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM runes_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(events, 7);
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![(s("1:1"), s("400"), s("1"), 2)]
        );
    })
    .await;
}

#[tokio::test]
async fn signet_hashes_start_at_173831() {
    with_database("signet", |name| async move {
        let mut module = RunesModule::new(config(&name, "signet"), journal())
            .await
            .unwrap();
        assert_eq!(module.start_height(), 0);
        let mint = RuneFeed {
            events: vec![RuneEvent {
                kind: RuneEventKind::Mint,
                txid: txid("a0"),
                outpoint: None,
                pkscript: None,
                rune_id: "5:1".into(),
                amount: 0,
            }],
            etchings: vec![etching("5:1", "a0", "SIGNETRUNE", 0)],
            counters: vec![counter("5:1", 0, 1)],
        };
        module.apply(&view(173_830, mint.clone())).await.unwrap();
        let mint_again = RuneFeed {
            etchings: Vec::new(),
            counters: vec![counter("5:1", 0, 2)],
            ..mint
        };
        module.apply(&view(173_831, mint_again)).await.unwrap();
        module
            .apply(&view(173_832, RuneFeed::default()))
            .await
            .unwrap();
        let mut conn = connect(&name).await;
        let heights: Vec<i32> =
            sqlx::query_scalar("SELECT block_height FROM runes_block_hashes ORDER BY block_height")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(heights, vec![173_830, 173_831, 173_832]);
        let hashes: Vec<(i32, String, String)> = sqlx::query(
            "SELECT block_height, block_event_hash, cumulative_event_hash \
             FROM runes_cumulative_event_hashes ORDER BY block_height",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
        let first = block_event_hash(&["mint;;;5:1;0".to_string()]);
        let second = block_event_hash(&[]);
        assert_eq!(
            hashes,
            vec![
                (173_831, first.clone(), first.clone()),
                (
                    173_832,
                    second.clone(),
                    cumulative_hash(Some(&first), &second)
                ),
            ]
        );
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM runes_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(events, 2);
    })
    .await;
}

#[tokio::test]
async fn mainnet_seed_row_exists_after_new() {
    with_database("seed", |name| async move {
        let mut module = RunesModule::new(config(&name, "mainnet"), journal())
            .await
            .unwrap();
        assert_eq!(module.start_height(), 840_000);
        let mut conn = connect(&name).await;
        let seed = sqlx::query(
            "SELECT rune_block, burned::text, divisibility, etching, terms_amount::text, \
             terms_cap::text, terms_height_l, terms_height_h, terms_offset_l, terms_offset_h, \
             mints::text, \"number\"::text, premine::text, rune_name, spacers, symbol, \
             extract(epoch from \"timestamp\")::int8, turbo, genesis_height, \
             last_updated_block_height FROM runes_id_to_entry WHERE rune_id = '1:0'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(seed.get::<i32, _>(0), 1);
        assert_eq!(seed.get::<String, _>(1), "0");
        assert_eq!(seed.get::<i32, _>(2), 0);
        assert_eq!(seed.get::<String, _>(3), "0".repeat(64));
        assert_eq!(seed.get::<Option<String>, _>(4).as_deref(), Some("1"));
        assert_eq!(
            seed.get::<Option<String>, _>(5).as_deref(),
            Some("340282366920938463463374607431768211455")
        );
        assert_eq!(seed.get::<Option<i64>, _>(6), Some(840_000));
        assert_eq!(seed.get::<Option<i64>, _>(7), Some(1_050_000));
        assert_eq!(seed.get::<Option<i64>, _>(8), None);
        assert_eq!(seed.get::<Option<i64>, _>(9), None);
        assert_eq!(seed.get::<String, _>(10), "0");
        assert_eq!(seed.get::<String, _>(11), "0");
        assert_eq!(seed.get::<String, _>(12), "0");
        assert_eq!(seed.get::<String, _>(13), "UNCOMMONGOODS");
        assert_eq!(seed.get::<i64, _>(14), 128);
        assert_eq!(seed.get::<Option<String>, _>(15).as_deref(), Some("⧉"));
        assert_eq!(seed.get::<i64, _>(16), 0);
        assert!(seed.get::<bool, _>(17));
        assert_eq!(seed.get::<i32, _>(18), 1);
        assert_eq!(seed.get::<i32, _>(19), 1);

        // A mint of 1:0 without any etching record updates the seed row.
        module
            .apply(&view(
                840_000,
                RuneFeed {
                    events: vec![
                        RuneEvent {
                            kind: RuneEventKind::Mint,
                            txid: txid("a0"),
                            outpoint: None,
                            pkscript: None,
                            rune_id: "1:0".into(),
                            amount: 1,
                        },
                        RuneEvent {
                            kind: RuneEventKind::Output,
                            txid: txid("a0"),
                            outpoint: Some(outpoint("a0", 0)),
                            pkscript: Some(pk1()),
                            rune_id: "1:0".into(),
                            amount: 1,
                        },
                    ],
                    etchings: Vec::new(),
                    counters: vec![counter("1:0", 0, 1)],
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![(s("1:0"), s("0"), s("1"), 840_000)]
        );

        // Reopening keeps the single seed row; a rollback below the mint
        // restores it.
        drop(module);
        let mut module = RunesModule::new(config(&name, "mainnet"), journal())
            .await
            .unwrap();
        module.rollback_to(839_999).await.unwrap();
        assert_eq!(
            entry_totals(&mut conn).await,
            vec![(s("1:0"), s("0"), s("0"), 1)]
        );
    })
    .await;
}

#[tokio::test]
async fn stored_indexer_version_matches_constant() {
    with_database("version", |name| async move {
        RunesModule::new(config(&name, "signet"), journal())
            .await
            .unwrap();
        let mut conn = connect(&name).await;
        let row = sqlx::query("SELECT indexer_version, db_version FROM runes_indexer_version")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>(0), opi_runes::INDEXER_VERSION);
        assert_eq!(row.get::<i32, _>(1), opi_runes::DB_VERSION);
        assert_eq!(opi_runes::INDEXER_VERSION, "opi-runes-full-node v1.0.0");
        assert_eq!(opi_runes::DB_VERSION, 6);
        assert_eq!(opi_runes::EVENT_HASH_VERSION, 2);
        let network: String = sqlx::query_scalar("SELECT network_type FROM runes_network_type")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(network, "signet");

        // Reopening on the same network works; another network is refused.
        RunesModule::new(config(&name, "signet"), journal())
            .await
            .unwrap();
        let error = RunesModule::new(config(&name, "mainnet"), journal())
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("network"), "{error:#}");
    })
    .await;
}
