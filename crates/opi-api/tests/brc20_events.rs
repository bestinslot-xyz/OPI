//! The brc20 event feeds, the balance history and the batch event lookup
//! over HTTP: `/v1/brc20/ticker/events`, `/v1/brc20/wallet/events`,
//! `/v1/brc20/deploys`, `/v1/brc20/wallet/balance_history` and
//! `/v1/brc20/events_by_inscriptions`.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters are checked before any query, and a valid request
//! answers the 500 body when its query fails. The seeded tests build a
//! throwaway database from opi-brc20's db_init.sql on the local Postgres
//! (127.0.0.1:5432, the current user), fill it with events and drop it
//! afterwards; they skip unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{INTERNAL_ERROR, JSON, app, expect, get, height, post},
    opi_api::{config::Module, db::Db},
    serde_json::{Value, json},
    sqlx::{
        Connection, PgConnection, PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    },
};

const BRC20: Module = Module::Brc20;

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

const CATEGORY: &str = "category must be one or more of deploy, mint, transfer, wrap, unwrap, brc20prog, separated by commas";
const WALLET_CATEGORY: &str = "category must be one or more of deploy, mint, transfer, wrap, unwrap, brc20prog, prog_transfer, separated by commas";

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(BRC20, 7.into());
    let limit = "limit must be an integer from 1 to 500";
    let before = "before_id must be an integer from 1 to 999999999999999999";
    for (uri, reason) in [
        ("/v1/brc20/ticker/events", "ticker is required"),
        ("/v1/brc20/ticker/events?ticker=", "ticker is required"),
        (
            "/v1/brc20/ticker/events?ticker=a&ticker=b",
            "ticker must be a single value",
        ),
        ("/v1/brc20/ticker/events?ticker=a&limit=0", limit),
        ("/v1/brc20/ticker/events?ticker=a&limit=501", limit),
        ("/v1/brc20/ticker/events?ticker=a&before_id=0", before),
        ("/v1/brc20/ticker/events?ticker=a&before_id=x", before),
        (
            "/v1/brc20/ticker/events?ticker=a&type[a]=x",
            "type must be a single value",
        ),
        ("/v1/brc20/ticker/events?ticker=a&category=nope", CATEGORY),
        ("/v1/brc20/ticker/events?ticker=a&category=wrap,", CATEGORY),
        ("/v1/brc20/wallet/events", "address or pkscript is required"),
        (
            "/v1/brc20/wallet/events?address=a&pkscript=00",
            "give address or pkscript, not both",
        ),
        (
            "/v1/brc20/wallet/events?pkscript=xyz",
            "pkscript must be hex of at most 20000 digits",
        ),
        (
            "/v1/brc20/ticker/events?ticker=a%00",
            "ticker must be at most 256 bytes without NUL",
        ),
        (
            "/v1/brc20/ticker/events?ticker=a&type=a%00",
            "type must be at most 256 bytes without NUL",
        ),
        (
            "/v1/brc20/wallet/events?address=a%00",
            "address must be at most 128 bytes without NUL",
        ),
        (
            "/v1/brc20/wallet/events?address=a&type=a%00",
            "type must be at most 256 bytes without NUL",
        ),
        ("/v1/brc20/wallet/events?address=a&limit=1000", limit),
        ("/v1/brc20/wallet/events?address=a&before_id=-5", before),
        (
            "/v1/brc20/wallet/events?address=a&category=Wrap",
            WALLET_CATEGORY,
        ),
        (
            "/v1/brc20/ticker/events?ticker=a&category=prog_transfer",
            CATEGORY,
        ),
        ("/v1/brc20/deploys?category=prog_transfer", CATEGORY),
        (
            "/v1/brc20/wallet/events?address=a&before=1",
            "before must be <block_height>:<rows>, the next_before of the previous page",
        ),
        ("/v1/brc20/deploys?category=deploys", CATEGORY),
        ("/v1/brc20/deploys?limit=0", limit),
        ("/v1/brc20/deploys?before_id=0", before),
        (
            "/v1/brc20/deploys?category=wrap&category=unwrap",
            "category must be a single value",
        ),
        (
            "/v1/brc20/wallet/balance_history?ticker=ordi",
            "address or pkscript is required",
        ),
        (
            "/v1/brc20/wallet/balance_history?address=a",
            "ticker is required",
        ),
        (
            "/v1/brc20/wallet/balance_history?pkscript=0&ticker=ordi",
            "pkscript must be hex of at most 20000 digits",
        ),
        (
            "/v1/brc20/wallet/balance_history?address=a&ticker=ordi&before_id=0",
            before,
        ),
        (
            "/v1/brc20/wallet/balance_history?address=a&ticker=ordi&limit=501",
            limit,
        ),
    ] {
        expect(&app, uri, 400, JSON, &bad(reason)).await;
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(BRC20, 7.into());
    for uri in [
        "/v1/brc20/ticker/events?ticker=ordi",
        "/v1/brc20/ticker/events?ticker=ordi&type=mint-inscribe&before_id=9&limit=500",
        "/v1/brc20/ticker/events?ticker=ordi&category=wrap,unwrap",
        "/v1/brc20/wallet/events?address=bc1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdp4gkthz",
        "/v1/brc20/wallet/events?pkscript=0014a1",
        "/v1/brc20/deploys",
        "/v1/brc20/deploys?category=wrap&before_id=5",
        "/v1/brc20/wallet/balance_history?pkscript=0014a1&ticker=ordi",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

/// Signet addresses and the P2WPKH pkScripts they encode.
const ALICE: &str = "tb1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdplwdcv3";
const BOB: &str = "tb1qk2et9v4jk2et9v4jk2et9v4jk2et9v4jayapc5";
const CAROL: &str = "tb1qc0pu8s7rc0pu8s7rc0pu8s7rc0pu8s7rrtn6n8";
const P_ALICE: &str = "0014a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const P_BOB: &str = "0014b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
const P_CAROL: &str = "0014c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";
/// Alice's pkScript in its mainnet form, which the signet data never names.
const ALICE_MAINNET: &str = "bc1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdp4gkthz";
/// A signet address with no events.
const NOBODY: &str = "tb1q6n2df4x56n2df4x56n2df4x56n2df4x5s4zrtg";
/// The BRC2.0 module's pkScript, the spent party of a deposit.
const MODULE: &str = "6a09425243323050524f47";
const ALICE_EVM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BOB_EVM: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const STRANGER_EVM: &str = "0x9999999999999999999999999999999999999999";
const ORDI_CONTRACT: &str = "0xc1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
const PEPE_CONTRACT: &str = "0xc2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2";

/// The BRC2.0 transaction hash whose bytes are all `byte`.
fn tx(byte: &str) -> String {
    format!("0x{}", byte.repeat(32))
}

/// One token in raw units.
const E18: &str = "000000000000000000";

/// The fixture. Events (ids ascend with the block height):
/// 1 predeploy (Alice), 2 deploy of ordi (Alice), 3 mint of ordi (Bob),
/// 4 mint of ordi (Alice), 5 transfer inscription of ordi (Alice),
/// 6 transfer of ordi from Alice to Bob, 7 mint of pepe (Bob), 8 deposit of
/// ordi into BRC2.0 (a transfer from Alice to the module's pkScript, above
/// the journal tip: no block time), 9 BRC2.0 call (Alice, no ticker), 10 a
/// BRC2.0 withdraw inscription of ordi (Alice) and 11 its withdrawal to Bob;
/// and light event 1001, a mint of ordi (Carol) in a block with no hash row.
/// A real database fills one of the two event tables, so the light id
/// continues the other's. The party rows come from the events, as the
/// indexer writes them. Alice's ordi balance history has four rows and
/// Bob's two.
///
/// BRC2.0 transfers (Alice and Bob have EVM addresses, the stranger none):
/// in block 301, tx 02 log 0 Alice to herself, tx 01 log 3 the stranger to
/// Alice, tx 01 log 2 the stranger to Bob and tx 01 log 1 Alice to Bob; in
/// block 250 (no events) tx 03 log 0 Bob to Alice in pepe; in block 50
/// (below every event) tx 04 log 2 Alice to the stranger.
fn fixture() -> String {
    let (tx1, tx2, tx3, tx4) = (tx("01"), tx("02"), tx("03"), tx("04"));
    format!(
        "INSERT INTO brc20_block_hashes (block_height, block_hash, block_time) VALUES
            (90, 'h90', 900), (100, 'h100', 1000), (120, 'h120', 1200),
            (200, 'h200', 2000), (300, 'h300', 3000), (301, 'h301', NULL);
         INSERT INTO brc20_tickers (original_tick, tick, max_supply, decimals, limit_per_mint,
                                    remaining_supply, burned_supply, is_self_mint,
                                    deploy_inscription_id, block_height)
         VALUES ('ORDI', 'ordi', 21000000{E18}, 18, 1000{E18}, 1000{E18}, 0, false, 'ordii0', 100),
                ('pepe', 'pepe', 1000{E18}, 8, 1000{E18}, 0, 0, false, 'pepei0', 200);
         INSERT INTO evm_addresses (evm_address, btc_address) VALUES ('{ALICE_EVM}', '{ALICE}'), ('{BOB_EVM}', '{BOB}');
         INSERT INTO brc20_prog_transfers (block_height, tx_hash, log_index, ticker, contract_address,
                                           from_address, to_address, amount)
         VALUES (301, '{tx2}', 0, 'ordi', '{ORDI_CONTRACT}', '{ALICE_EVM}', '{ALICE_EVM}', 1{E18}),
                (301, '{tx1}', 3, 'ordi', '{ORDI_CONTRACT}', '{STRANGER_EVM}', '{ALICE_EVM}', 3{E18}),
                (301, '{tx1}', 2, 'ordi', '{ORDI_CONTRACT}', '{STRANGER_EVM}', '{BOB_EVM}', 4{E18}),
                (301, '{tx1}', 1, 'ordi', '{ORDI_CONTRACT}', '{ALICE_EVM}', '{BOB_EVM}', 2{E18}),
                (250, '{tx3}', 0, 'pepe', '{PEPE_CONTRACT}', '{BOB_EVM}', '{ALICE_EVM}', 7{E18}),
                (50, '{tx4}', 2, 'ordi', '{ORDI_CONTRACT}', '{ALICE_EVM}', '{STRANGER_EVM}', 1{E18});
         INSERT INTO brc20_events (id, event_type, block_height, inscription_id, event)
         VALUES
          (1, 12, 90, 'pre0', '{{\"predeployer_pkScript\":\"{P_ALICE}\",\"predeployer_wallet\":\"{ALICE}\",\"hash\":\"hh\",\"block_height\":\"95\"}}'),
          (2, 0, 100, 'ordii0', '{{\"deployer_pkScript\":\"{P_ALICE}\",\"deployer_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"decimals\":\"18\",\"max_supply\":\"21000000{E18}\",\"limit_per_mint\":\"1000{E18}\",\"is_self_mint\":\"false\"}}'),
          (3, 1, 120, 'm1i0', '{{\"minted_pkScript\":\"{P_BOB}\",\"minted_wallet\":\"{BOB}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"1000{E18}\",\"parent_id\":\"\"}}'),
          (4, 1, 120, 'm2i0', '{{\"minted_pkScript\":\"{P_ALICE}\",\"minted_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"500{E18}\",\"parent_id\":\"\"}}'),
          (5, 2, 200, 't1i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"100{E18}\"}}'),
          (6, 3, 200, 't1i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"spent_pkScript\":\"{P_BOB}\",\"spent_wallet\":\"{BOB}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"100{E18}\",\"using_tx_id\":\"u\"}}'),
          (7, 1, 300, 'm3i0', '{{\"minted_pkScript\":\"{P_BOB}\",\"minted_wallet\":\"{BOB}\",\"tick\":\"pepe\",\"original_tick\":\"pepe\",\"amount\":\"7{E18}\",\"parent_id\":\"\"}}'),
          (8, 3, 301, 't2i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"spent_pkScript\":\"{MODULE}\",\"spent_wallet\":null,\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"50{E18}\",\"using_tx_id\":\"u2\"}}'),
          (9, 6, 301, 'c1i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"contract_address\":\"0x1111111111111111111111111111111111111111\",\"data\":null}}'),
          (10, 8, 301, 'w1i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"20{E18}\"}}'),
          (11, 9, 301, 'w1i0', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"spent_pkScript\":\"{P_BOB}\",\"spent_wallet\":\"{BOB}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"20{E18}\"}}');
         INSERT INTO brc20_light_events (id, event_type, block_height, inscription_id, event)
         VALUES (1001, 1, 310, 'm4i0', '{{\"minted_pkScript\":\"{P_CAROL}\",\"minted_wallet\":\"{CAROL}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"9{E18}\",\"parent_id\":\"\"}}');
         INSERT INTO brc20_event_parties (pkscript, event_id)
         SELECT k, e.id FROM brc20_events e CROSS JOIN LATERAL unnest(brc20_event_party_pkscripts(e.event)) k;
         INSERT INTO brc20_event_parties (pkscript, event_id)
         SELECT k, e.id FROM brc20_light_events e CROSS JOIN LATERAL unnest(brc20_event_party_pkscripts(e.event)) k;
         INSERT INTO brc20_historic_balances (id, pkscript, wallet, tick, overall_balance, available_balance, block_height)
         VALUES (1, '{P_ALICE}', '{ALICE}', 'ordi', 500, 500, 120),
                (2, '{P_BOB}', '{BOB}', 'ordi', 1000, 1000, 120),
                (3, '{P_ALICE}', '{ALICE}', 'ordi', 500, 400, 200),
                (4, '{P_ALICE}', '{ALICE}', 'ordi', 400, 400, 200),
                (5, '{P_BOB}', '{BOB}', 'ordi', 1100, 1100, 200),
                (6, '{P_ALICE}', '{ALICE}', 'ordi', 350, 350, 301);"
    )
}

/// Runs `body` with the app and a pool on a throwaway database seeded with
/// [`fixture`], dropped even when `body` panics.
async fn with_seeded<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Router, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let user = std::env::var("USER").unwrap();
    let options = || {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username(&user)
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("opi_api_events_{tag}_{}_{nanos}", std::process::id());
    let mut root = PgConnection::connect_with(&options().database("postgres"))
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut root)
        .await
        .unwrap();
    let pool = PgPoolOptions::new().max_connections(2).connect_lazy_with(
        options()
            .database(&name)
            .options([("plan_cache_mode", opi_api::db::PLAN_CACHE_MODE)]),
    );
    sqlx::raw_sql(include_str!("../../opi-brc20/src/database/sql/db_init.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(fixture()))
        .execute(&pool)
        .await
        .unwrap();
    let router = app(BRC20, Db::from_pool(pool.clone()));
    let result = tokio::spawn(body(router, pool.clone())).await;
    pool.close().await;
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

fn ok(result: &str) -> String {
    format!(r#"{{"error":null,"result":{result}}}"#)
}

/// A page's row ids in order, its `has_more` and `next_before_id`.
async fn feed(app: &Router, uri: &str) -> (Vec<u32>, bool, Option<u32>) {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    let ids = answer
        .body
        .split(r#"{"id":"#)
        .skip(1)
        .map(|rest| rest[..rest.find(',').unwrap()].parse().unwrap())
        .collect();
    let next = answer
        .body
        .split(r#""next_before_id":"#)
        .nth(1)
        .map(|rest| &rest[..rest.find(',').unwrap()])
        .and_then(|text| text.parse().ok());
    (ids, answer.body.contains(r#""has_more":true"#), next)
}

#[tokio::test]
async fn seeded_ticker_events_page_newest_first_across_both_tables() {
    with_seeded("ticker", |app, _| async move {
        // the light event 1001 leads the full table's ordi events
        let all = vec![1001, 11, 10, 8, 6, 5, 4, 3, 2];
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi").await,
            (all.clone(), false, None)
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ORDI").await.0,
            all
        );
        // a key-paged walk repeats and skips nothing
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&limit=2").await,
            (vec![1001, 11], true, Some(11))
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&limit=3&before_id=11").await,
            (vec![10, 8, 6], true, Some(6))
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&before_id=6").await,
            (vec![5, 4, 3, 2], false, None)
        );
        // a page of exactly the rest has no more
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&limit=2&before_id=4").await,
            (vec![3, 2], false, None)
        );
        // event type filter
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&type=mint-inscribe").await,
            (vec![1001, 4, 3], false, None)
        );
        assert_eq!(
            feed(
                &app,
                "/v1/brc20/ticker/events?ticker=ordi&type=mint-inscribe&limit=2&before_id=1001"
            )
            .await,
            (vec![4, 3], false, None)
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&type=predeploy-inscribe").await,
            (vec![], false, None)
        );
        // a ticker with one event, with its block time
        expect(
            &app,
            "/v1/brc20/ticker/events?ticker=pepe",
            200,
            JSON,
            &ok(&format!(r#"{{"ticker":"pepe","decimals":18,"ticker_decimals":8,"event_type":null,"events":[{{"id":7,"block_height":300,"block_time":3000,"inscription_id":"m3i0","event_type":"mint-inscribe","ticker_decimals":8,"event":{{"tick":"pepe","amount":"7000000000000000000","parent_id":"","minted_wallet":"{BOB}","original_tick":"pepe","minted_pkScript":"{P_BOB}"}}}}],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}}"#)),
        )
        .await;
        // an unknown ticker, and an unknown type with the names to use
        expect(
            &app,
            "/v1/brc20/ticker/events?ticker=nope",
            404,
            JSON,
            &bad("ticker not found"),
        )
        .await;
        let answer = get(&app, "/v1/brc20/ticker/events?ticker=ordi&type=mint").await;
        assert_eq!(answer.status, 400);
        assert!(
            answer.body.starts_with(
                r#"{"error":"type must be one of deploy-inscribe, mint-inscribe, transfer-inscribe, "#
            ) && answer.body.ends_with(r#"predeploy-inscribe","result":null}"#),
            "{}",
            answer.body
        );
    })
    .await;
}

#[tokio::test]
async fn seeded_ticker_events_filter_by_category() {
    with_seeded("tickercat", |app, _| async move {
        let base = "/v1/brc20/ticker/events?ticker=ordi";
        for (category, ids) in [
            ("deploy", vec![2]),
            ("mint", vec![1001, 4, 3]),
            ("transfer", vec![6, 5]),
            ("wrap", vec![8]),
            ("unwrap", vec![11, 10]),
            // BRC2.0 deploys, calls and transactions carry no ticker
            ("brc20prog", vec![]),
            ("wrap,unwrap", vec![11, 10, 8]),
            ("unwrap,wrap,wrap", vec![11, 10, 8]),
            (
                "deploy,mint,transfer,wrap,unwrap,brc20prog",
                vec![1001, 11, 10, 8, 6, 5, 4, 3, 2],
            ),
        ] {
            assert_eq!(
                feed(&app, &format!("{base}&category={category}")).await,
                (ids, false, None),
                "{category}"
            );
        }
        // with a type, an event passes both
        assert_eq!(
            feed(
                &app,
                &format!("{base}&category=transfer&type=transfer-transfer")
            )
            .await
            .0,
            vec![6]
        );
        assert_eq!(
            feed(&app, &format!("{base}&category=wrap&type=mint-inscribe"))
                .await
                .0,
            Vec::<u32>::new()
        );
        assert_eq!(
            feed(
                &app,
                &format!("{base}&category=unwrap&type=brc20prog-withdraw-transfer")
            )
            .await
            .0,
            vec![11]
        );
        // a category page pages by key
        assert_eq!(
            feed(&app, &format!("{base}&category=mint&limit=1")).await,
            (vec![1001], true, Some(1001))
        );
        assert_eq!(
            feed(
                &app,
                &format!("{base}&category=mint&limit=1&before_id=1001")
            )
            .await,
            (vec![4], true, Some(4))
        );
        // an empty category is no filter
        assert_eq!(feed(&app, &format!("{base}&category=")).await.0.len(), 9);
    })
    .await;
}

#[tokio::test]
async fn seeded_events_carry_block_time_or_null() {
    with_seeded("time", |app, _| async move {
        // 8 to 11 are above the journal tip (NULL time), 1001 is in a block
        // with no hash row
        let answer = get(&app, "/v1/brc20/ticker/events?ticker=ordi").await;
        let times: Vec<&str> = answer
            .body
            .split(r#""block_time":"#)
            .skip(1)
            .map(|rest| &rest[..rest.find(',').unwrap()])
            .collect();
        assert_eq!(
            times,
            ["null", "null", "null", "null", "2000", "2000", "1200", "1200", "1000"]
        );
        // the deposit's raw event: a transfer spent to the module's pkScript
        let answer = get(&app, "/v1/brc20/ticker/events?ticker=ordi&type=transfer-transfer&limit=1").await;
        assert!(
            answer.body.contains(&format!(
                r#"{{"id":8,"block_height":301,"block_time":null,"inscription_id":"t2i0","event_type":"transfer-transfer","ticker_decimals":18,"event":{{"tick":"ordi","amount":"50000000000000000000","using_tx_id":"u2","spent_wallet":null,"original_tick":"ORDI","source_wallet":"{ALICE}","spent_pkScript":"{MODULE}","source_pkScript":"{P_ALICE}"}}}}"#
            )),
            "{}",
            answer.body
        );
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_events_match_any_party() {
    with_seeded("wallet", |app, _| async move {
        // Alice: predeploy, her mint, the transfer inscription, the transfer,
        // the deposit, the call and the withdrawal (the deploy is hers too)
        let alice = vec![11, 10, 9, 8, 6, 5, 4, 2, 1];
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?address={ALICE}")).await,
            (alice.clone(), false, None)
        );
        // by pkScript, in any case
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?pkscript={P_ALICE}")).await.0,
            alice
        );
        assert_eq!(
            feed(
                &app,
                &format!("/v1/brc20/wallet/events?pkscript={}", P_ALICE.to_uppercase())
            )
            .await
            .0,
            alice
        );
        // by the EVM address Alice's Bitcoin address maps to
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?address={ALICE_EVM}")).await.0,
            alice
        );
        assert_eq!(
            feed(
                &app,
                &format!("/v1/brc20/wallet/events?address={}", ALICE_EVM.to_uppercase().replace("0X", "0x"))
            )
            .await
            .0,
            alice
        );
        // Bob appears as a minter, and as the spent party of Alice's
        // transfer and withdrawal
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?address={BOB}")).await.0,
            vec![11, 7, 6, 3]
        );
        // the module's pkScript sees the deposit
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?pkscript={MODULE}")).await.0,
            vec![8]
        );
        // a light event
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?address={CAROL}")).await,
            (vec![1001], false, None)
        );
        // the same pkScript in another network's form, or in capitals, is
        // not the address the events name
        for address in [ALICE_MAINNET.to_string(), ALICE.to_uppercase()] {
            assert_eq!(
                feed(&app, &format!("/v1/brc20/wallet/events?address={address}")).await,
                (vec![], false, None),
                "{address}"
            );
        }
        // an EVM address with no Bitcoin address, a wallet with no events and
        // a string that is no address
        expect(
            &app,
            &format!("/v1/brc20/wallet/events?address={STRANGER_EVM}"),
            200,
            JSON,
            &ok(&format!(r#"{{"address":"{STRANGER_EVM}","pkscript":null,"decimals":18,"event_type":null,"events":[],"limit":100,"has_more":false,"next_before_id":null,"next_before":null,"block_height":301}}"#)),
        )
        .await;
        for address in [NOBODY, "tb1qnobody"] {
            expect(
                &app,
                &format!("/v1/brc20/wallet/events?address={address}"),
                200,
                JSON,
                &ok(&format!(r#"{{"address":"{address}","pkscript":null,"decimals":18,"event_type":null,"events":[],"limit":100,"has_more":false,"next_before_id":null,"next_before":null,"block_height":301}}"#)),
            )
            .await;
        }
    })
    .await;
}

/// The wallet feed lists, for every party in the data, the events the
/// party's `*_pkScript` or `*_wallet` fields name: the parties table gives
/// the same events as matching the event objects.
#[tokio::test]
async fn seeded_wallet_events_are_the_events_naming_the_wallet() {
    with_seeded("walletsame", |app, pool| async move {
        let parties: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT DISTINCT e.event ->> (p || '_pkScript'), e.event ->> (p || '_wallet')
             FROM (SELECT event FROM brc20_events UNION ALL SELECT event FROM brc20_light_events) e,
                  unnest(ARRAY['deployer', 'minted', 'source', 'spent', 'predeployer']) p
             WHERE e.event ->> (p || '_pkScript') IS NOT NULL",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(parties.len() >= 4, "{parties:?}");
        for (pkscript, wallet) in parties {
            for (param, key) in [("pkscript", Some(pkscript.clone())), ("address", wallet)] {
                let Some(key) = key else { continue };
                let expected: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM (SELECT id, event FROM brc20_events
                                     UNION ALL SELECT id, event FROM brc20_light_events) e
                     WHERE $1 IN (e.event ->> 'deployer_pkScript', e.event ->> 'minted_pkScript',
                                  e.event ->> 'source_pkScript', e.event ->> 'spent_pkScript',
                                  e.event ->> 'predeployer_pkScript', e.event ->> 'deployer_wallet',
                                  e.event ->> 'minted_wallet', e.event ->> 'source_wallet',
                                  e.event ->> 'spent_wallet', e.event ->> 'predeployer_wallet')
                     ORDER BY id DESC",
                )
                .bind(&key)
                .fetch_all(&pool)
                .await
                .unwrap();
                let listed = feed(&app, &format!("/v1/brc20/wallet/events?{param}={key}"))
                    .await
                    .0;
                let listed: Vec<i64> = listed.into_iter().map(i64::from).collect();
                assert_eq!(listed, expected, "{param}={key}");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_events_page_and_filter() {
    with_seeded("walletpage", |app, _| async move {
        let base = format!("/v1/brc20/wallet/events?address={ALICE}");
        assert_eq!(
            feed(&app, &format!("{base}&limit=3")).await,
            (vec![11, 10, 9], true, Some(9))
        );
        assert_eq!(
            feed(&app, &format!("{base}&limit=3&before_id=9")).await,
            (vec![8, 6, 5], true, Some(5))
        );
        assert_eq!(
            feed(&app, &format!("{base}&limit=3&before_id=5")).await,
            (vec![4, 2, 1], false, None)
        );
        assert_eq!(
            feed(&app, &format!("{base}&type=transfer-transfer")).await,
            (vec![8, 6], false, None)
        );
        assert_eq!(
            feed(&app, &format!("{base}&type=brc20prog-call-inscribe")).await,
            (vec![9], false, None)
        );
        // categories
        for (category, ids) in [
            ("deploy", vec![2, 1]),
            ("mint", vec![4]),
            ("transfer", vec![6, 5]),
            ("wrap", vec![8]),
            ("unwrap", vec![11, 10]),
            ("brc20prog", vec![9]),
            ("wrap,unwrap", vec![11, 10, 8]),
        ] {
            assert_eq!(
                feed(&app, &format!("{base}&category={category}")).await,
                (ids, false, None),
                "{category}"
            );
        }
        assert_eq!(
            feed(&app, &format!("{base}&category=deploy&limit=1")).await,
            (vec![2], true, Some(2))
        );
        assert_eq!(
            feed(
                &app,
                &format!("/v1/brc20/wallet/events?address={BOB}&category=unwrap")
            )
            .await
            .0,
            vec![11]
        );
        assert_eq!(
            feed(&app, &format!("{base}&category=transfer&type=transfer-inscribe")).await.0,
            vec![5]
        );
        // the call carries no ticker, so its ticker_decimals is null; the
        // wallet's own fields echo the request
        let answer = get(&app, &format!("{base}&type=brc20prog-call-inscribe")).await;
        assert!(
            answer.body.starts_with(&format!(
                r#"{{"error":null,"result":{{"address":"{ALICE}","pkscript":null,"decimals":18,"event_type":"brc20prog-call-inscribe","events":[{{"id":9,"block_height":301,"block_time":null,"inscription_id":"c1i0","event_type":"brc20prog-call-inscribe","ticker_decimals":null,"#
            )),
            "{}",
            answer.body
        );
        let answer = get(&app, &format!("{base}&type=nope")).await;
        assert_eq!(answer.status, 400);
    })
    .await;
}

/// A wallet feed page: each row's label (an event's id, a transfer's
/// `<first tx byte>:<log index>`), each row's id-free label (an event's
/// inscription id and type instead of its id), `has_more` and
/// `next_before`. The page's body must parse and its result carry
/// `next_before`.
async fn wallet_page(app: &Router, uri: &str) -> (Vec<String>, Vec<String>, bool, Option<String>) {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
    let result = &body["result"];
    let mut labels = Vec::new();
    let mut content = Vec::new();
    for row in result["events"].as_array().unwrap() {
        if row["event_type"] == "brc20prog-token-transfer" {
            let event = &row["event"];
            let label = format!(
                "{}:{}",
                &event["tx_hash"].as_str().unwrap()[2..4],
                event["log_index"]
            );
            labels.push(label.clone());
            content.push(label);
        } else {
            labels.push(row["id"].to_string());
            content.push(format!(
                "{}/{}",
                row["inscription_id"].as_str().unwrap(),
                row["event_type"].as_str().unwrap()
            ));
        }
    }
    let next_before = result
        .get("next_before")
        .unwrap_or_else(|| panic!("{uri}: no next_before: {}", answer.body));
    (
        labels,
        content,
        result["has_more"].as_bool().unwrap(),
        next_before.as_str().map(String::from),
    )
}

/// Alice's events and BRC2.0 transfers, newest block first; in a block, the
/// events by id descending, then the transfers by transaction hash and log
/// index descending.
const ALICE_ALL: [&str; 14] = [
    "11", "10", "9", "8", "02:0", "01:3", "01:1", "03:0", "6", "5", "4", "2", "1", "04:2",
];

#[tokio::test]
async fn seeded_wallet_events_leave_transfers_out_by_default() {
    with_seeded("progdefault", |app, _| async move {
        // the answer is the one the route gave before BRC2.0 transfers, with
        // the block cursor beside next_before_id
        let base = format!("/v1/brc20/wallet/events?address={ALICE}");
        for uri in [
            base.clone(),
            format!("{base}&include_prog_transfers=false"),
            format!("{base}&include_prog_transfers="),
        ] {
            assert_eq!(
                wallet_page(&app, &uri).await.0,
                ["11", "10", "9", "8", "6", "5", "4", "2", "1"],
                "{uri}"
            );
        }
        expect(
            &app,
            &format!("{base}&category=deploy&limit=1"),
            200,
            JSON,
            &ok(&format!(r#"{{"address":"{ALICE}","pkscript":null,"decimals":18,"event_type":null,"events":[{{"id":2,"block_height":100,"block_time":1000,"inscription_id":"ordii0","event_type":"deploy-inscribe","ticker_decimals":18,"event":{{"tick":"ordi","decimals":"18","max_supply":"21000000{E18}","is_self_mint":"false","original_tick":"ORDI","limit_per_mint":"1000{E18}","deployer_wallet":"{ALICE}","deployer_pkScript":"{P_ALICE}"}}}}],"limit":1,"has_more":true,"next_before_id":2,"next_before":"100:1","block_height":301}}"#)),
        )
        .await;
        expect(
            &app,
            &format!("/v1/brc20/wallet/events?address={STRANGER_EVM}"),
            200,
            JSON,
            &ok(&format!(r#"{{"address":"{STRANGER_EVM}","pkscript":null,"decimals":18,"event_type":null,"events":[],"limit":100,"has_more":false,"next_before_id":null,"next_before":null,"block_height":301}}"#)),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_events_mix_in_prog_transfers() {
    with_seeded("progmix", |app, _| async move {
        let all: Vec<String> = ALICE_ALL.map(String::from).to_vec();
        // by address, pkScript and EVM address alike
        for wallet in [
            format!("address={ALICE}"),
            format!("pkscript={P_ALICE}"),
            format!("address={ALICE_EVM}"),
        ] {
            let page = wallet_page(
                &app,
                &format!("/v1/brc20/wallet/events?{wallet}&include_prog_transfers=true"),
            )
            .await;
            assert_eq!((page.0, page.2, page.3), (all.clone(), false, None), "{wallet}");
        }
        let base = format!("/v1/brc20/wallet/events?address={ALICE}");
        // the category alone, and beside an event category
        assert_eq!(
            wallet_page(&app, &format!("{base}&category=prog_transfer")).await.0,
            ["02:0", "01:3", "01:1", "03:0", "04:2"]
        );
        assert_eq!(
            wallet_page(&app, &format!("{base}&category=mint,prog_transfer")).await.0,
            ["02:0", "01:3", "01:1", "03:0", "4", "04:2"]
        );
        assert_eq!(
            wallet_page(&app, &format!("{base}&category=mint&include_prog_transfers=true"))
                .await
                .0,
            ["02:0", "01:3", "01:1", "03:0", "4", "04:2"]
        );
        // a type keeps events of that type only
        assert_eq!(
            wallet_page(&app, &format!("{base}&type=mint-inscribe&include_prog_transfers=true"))
                .await
                .0,
            ["4"]
        );
        // a transfer row: the wallet's side and the counterparty's
        let answer = get(&app, &format!("{base}&category=prog_transfer&limit=3")).await;
        let (tx1, tx2) = (tx("01"), tx("02"));
        assert_eq!(
            answer.body,
            ok(&format!(
                r#"{{"address":"{ALICE}","pkscript":null,"decimals":18,"event_type":null,"events":[{{"id":null,"block_height":301,"block_time":null,"inscription_id":null,"event_type":"brc20prog-token-transfer","ticker_decimals":18,"event":{{"tick":"ordi","amount":"1{E18}","tx_hash":"{tx2}","log_index":0,"contract_address":"{ORDI_CONTRACT}","from":"{ALICE_EVM}","to":"{ALICE_EVM}","direction":"out","counterparty":{{"evm_address":"{ALICE_EVM}","address":"{ALICE}"}}}}}},{{"id":null,"block_height":301,"block_time":null,"inscription_id":null,"event_type":"brc20prog-token-transfer","ticker_decimals":18,"event":{{"tick":"ordi","amount":"3{E18}","tx_hash":"{tx1}","log_index":3,"contract_address":"{ORDI_CONTRACT}","from":"{STRANGER_EVM}","to":"{ALICE_EVM}","direction":"in","counterparty":{{"evm_address":"{STRANGER_EVM}","address":null}}}}}},{{"id":null,"block_height":301,"block_time":null,"inscription_id":null,"event_type":"brc20prog-token-transfer","ticker_decimals":18,"event":{{"tick":"ordi","amount":"2{E18}","tx_hash":"{tx1}","log_index":1,"contract_address":"{ORDI_CONTRACT}","from":"{ALICE_EVM}","to":"{BOB_EVM}","direction":"out","counterparty":{{"evm_address":"{BOB_EVM}","address":"{BOB}"}}}}}}],"limit":3,"has_more":true,"next_before_id":null,"next_before":"301:3","block_height":301}}"#
            ))
        );
        // Bob's pepe receipt in a block with a time and another precision
        let answer = get(&app, &format!("/v1/brc20/wallet/events?address={BOB}&category=prog_transfer")).await;
        assert!(
            answer.body.contains(r#""block_height":250,"block_time":null,"inscription_id":null,"event_type":"brc20prog-token-transfer","ticker_decimals":8,"event":{"tick":"pepe""#),
            "{}",
            answer.body
        );
        // a Bitcoin address without an EVM address has its events only
        let page = wallet_page(
            &app,
            &format!("/v1/brc20/wallet/events?address={CAROL}&include_prog_transfers=true"),
        )
        .await;
        assert_eq!((page.0, page.2, page.3), (vec!["1001".to_string()], false, None));
        // an EVM address with no Bitcoin address has its transfers
        assert_eq!(
            wallet_page(
                &app,
                &format!("/v1/brc20/wallet/events?address={STRANGER_EVM}&include_prog_transfers=true")
            )
            .await
            .0,
            ["01:3", "01:2", "04:2"]
        );
    })
    .await;
}

/// Walks Alice's feed page by page with `before`, `limit` rows a page, and
/// answers each page's request and id-free labels.
async fn walk(
    app: &Router,
    query: &str,
    limit: usize,
) -> Vec<(String, Vec<String>, Option<String>)> {
    let base = format!("/v1/brc20/wallet/events?address={ALICE}&{query}&limit={limit}");
    let mut pages = Vec::new();
    let mut uri = base.clone();
    loop {
        let (_, content, has_more, next) = wallet_page(app, &uri).await;
        assert!(content.len() <= limit, "{uri}");
        assert_eq!(has_more, next.is_some(), "{uri}");
        pages.push((uri.clone(), content, next.clone()));
        match next {
            Some(next) => uri = format!("{base}&before={next}"),
            None => return pages,
        }
    }
}

#[tokio::test]
async fn seeded_wallet_events_page_by_block_cursor() {
    with_seeded("progpage", |app, pool| async move {
        let base = format!("/v1/brc20/wallet/events?address={ALICE}&include_prog_transfers=true");
        // block 301 splits across pages; next_before counts the block's rows
        // across pages
        for (before, expected, next) in [
            ("", vec!["11", "10"], Some("301:2")),
            ("&before=301:2", vec!["9", "8"], Some("301:4")),
            ("&before=301:4", vec!["02:0", "01:3"], Some("301:6")),
            ("&before=301:6", vec!["01:1", "03:0"], Some("250:1")),
            ("&before=250:1", vec!["6", "5"], Some("200:2")),
            ("&before=200:2", vec!["4", "2"], Some("100:1")),
            ("&before=100:1", vec!["1", "04:2"], None),
            // a cursor past a block's rows starts below it; one above the
            // tip starts at the top
            ("&before=301:99", vec!["03:0", "6"], Some("200:1")),
            ("&before=900:0", vec!["11", "10"], Some("301:2")),
            ("&before=0:0", vec![], None),
        ] {
            let page = wallet_page(&app, &format!("{base}&limit=2{before}")).await;
            assert_eq!(
                (page.0, page.3),
                (expected.iter().map(|s| s.to_string()).collect(), next.map(String::from)),
                "{before}"
            );
        }
        // every page size walks the feed without a repeat or a gap
        let all: Vec<String> = ALICE_ALL.map(String::from).to_vec();
        let mut walks = Vec::new();
        for limit in 1..=15 {
            let pages = walk(&app, "include_prog_transfers=true", limit).await;
            let rows: Vec<String> = pages.iter().flat_map(|(_, rows, _)| rows.clone()).collect();
            let (_, expected, _, _) = wallet_page(&app, &format!("{base}&limit=500")).await;
            assert_eq!(rows, expected, "limit {limit}");
            assert_eq!(rows.len(), all.len());
            walks.push(pages);
        }
        // the events alone page by block too, in the order of one page
        let (_, events, _, _) = wallet_page(
            &app,
            &format!("/v1/brc20/wallet/events?address={ALICE}&type=&limit=500"),
        )
        .await;
        assert_eq!(events.len(), 9);
        for limit in 1..=4 {
            let rows: Vec<String> = walk(&app, "type=", limit)
                .await
                .into_iter()
                .flat_map(|(_, rows, _)| rows)
                .collect();
            assert_eq!(rows, events, "limit {limit}");
        }
        // the same cursors on a database whose event ids differ (gaps
        // within and between blocks) answer the same pages
        sqlx::raw_sql(
            "UPDATE brc20_light_events SET id = id + 2000;
             UPDATE brc20_event_parties SET event_id = event_id + 2000 WHERE event_id = 1001;
             UPDATE brc20_events SET id = id + 1000 WHERE id >= 10;
             UPDATE brc20_events SET id = id + 50 WHERE id BETWEEN 4 AND 9;
             UPDATE brc20_event_parties SET event_id = event_id + 1000 WHERE event_id BETWEEN 10 AND 11;
             UPDATE brc20_event_parties SET event_id = event_id + 50 WHERE event_id BETWEEN 4 AND 9;",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            wallet_page(&app, &format!("{base}&limit=2")).await.0,
            ["1011", "1010"]
        );
        for pages in walks {
            for (uri, content, next) in pages {
                let (_, again, _, next_again) = wallet_page(&app, &uri).await;
                assert_eq!((again, next_again), (content, next), "{uri}");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_events_refuse_a_bad_block_cursor() {
    with_seeded("progbad", |app, _| async move {
        let base = format!("/v1/brc20/wallet/events?address={ALICE}");
        let cursor = "before must be <block_height>:<rows>, the next_before of the previous page";
        for (query, reason) in [
            ("before=", cursor),
            ("before=abc", cursor),
            ("before=301", cursor),
            ("before=301:", cursor),
            ("before=:2", cursor),
            ("before=-1:2", cursor),
            ("before=301:-2", cursor),
            ("before=301:2:3", cursor),
            ("before=2147483648:0", cursor),
            ("before=301:1000000000000000000", cursor),
            ("before=1&before=2", "before must be a single value"),
            (
                "before=301:2&before_id=5",
                "give before or before_id, not both",
            ),
            (
                "before_id=5&include_prog_transfers=true",
                "before_id does not page BRC2.0 transfers; use before",
            ),
            (
                "before_id=5&category=prog_transfer",
                "before_id does not page BRC2.0 transfers; use before",
            ),
            (
                "include_prog_transfers=yes",
                "include_prog_transfers must be true or false",
            ),
            ("category=prog_transfers", WALLET_CATEGORY),
        ] {
            expect(&app, &format!("{base}&{query}"), 400, JSON, &bad(reason)).await;
        }
        // a before_id page still gives the block cursor of its last row
        let page = wallet_page(&app, &format!("{base}&before_id=10&limit=2")).await;
        assert_eq!(
            (page.0, page.3),
            (
                vec!["9".to_string(), "8".to_string()],
                Some("301:4".to_string())
            )
        );
    })
    .await;
}

#[tokio::test]
async fn seeded_deploys_list_the_network_by_category() {
    with_seeded("deploys", |app, _| async move {
        // every deploy and predeploy by default
        assert_eq!(
            feed(&app, "/v1/brc20/deploys").await,
            (vec![2, 1], false, None)
        );
        assert_eq!(
            feed(&app, "/v1/brc20/deploys?limit=1").await,
            (vec![2], true, Some(2))
        );
        assert_eq!(
            feed(&app, "/v1/brc20/deploys?limit=1&before_id=2").await,
            (vec![1], false, None)
        );
        assert_eq!(
            feed(&app, "/v1/brc20/deploys?type=predeploy-inscribe").await.0,
            vec![1]
        );
        for (category, ids) in [
            ("wrap", vec![8]),
            ("unwrap", vec![11, 10]),
            ("brc20prog", vec![9]),
            ("mint", vec![1001, 7, 4, 3]),
            ("transfer", vec![6, 5]),
            ("deploy,wrap", vec![8, 2, 1]),
        ] {
            assert_eq!(
                feed(&app, &format!("/v1/brc20/deploys?category={category}")).await,
                (ids, false, None),
                "{category}"
            );
        }
        assert_eq!(
            feed(&app, "/v1/brc20/deploys?category=unwrap&type=brc20prog-withdraw-transfer").await.0,
            vec![11]
        );
        expect(
            &app,
            "/v1/brc20/deploys?category=wrap,deploy&type=transfer-transfer",
            200,
            JSON,
            &ok(&format!(r#"{{"category":["deploy","wrap"],"decimals":18,"event_type":"transfer-transfer","events":[{{"id":8,"block_height":301,"block_time":null,"inscription_id":"t2i0","event_type":"transfer-transfer","ticker_decimals":18,"event":{{"tick":"ordi","amount":"50000000000000000000","using_tx_id":"u2","spent_wallet":null,"original_tick":"ORDI","source_wallet":"{ALICE}","spent_pkScript":"{MODULE}","source_pkScript":"{P_ALICE}"}}}}],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}}"#)),
        )
        .await;
        let answer = get(&app, "/v1/brc20/deploys").await;
        assert!(
            answer.body.starts_with(r#"{"error":null,"result":{"category":["deploy"],"decimals":18,"event_type":null,"events":[{"id":2,"#),
            "{}",
            answer.body
        );
    })
    .await;
}

#[tokio::test]
async fn seeded_balance_history_pages_newest_first() {
    with_seeded("history", |app, _| async move {
        let base = format!("/v1/brc20/wallet/balance_history?address={ALICE}&ticker=ordi");
        assert_eq!(feed(&app, &base).await, (vec![6, 4, 3, 1], false, None));
        // by pkScript, by EVM address, and in any ticker case
        for uri in [
            format!("/v1/brc20/wallet/balance_history?pkscript={P_ALICE}&ticker=ordi"),
            format!("/v1/brc20/wallet/balance_history?address={ALICE_EVM}&ticker=ordi"),
            format!("/v1/brc20/wallet/balance_history?address={ALICE}&ticker=ORDI"),
        ] {
            assert_eq!(feed(&app, &uri).await.0, vec![6, 4, 3, 1], "{uri}");
        }
        // a key-paged walk repeats and skips nothing
        assert_eq!(
            feed(&app, &format!("{base}&limit=2")).await,
            (vec![6, 4], true, Some(4))
        );
        assert_eq!(
            feed(&app, &format!("{base}&limit=2&before_id=4")).await,
            (vec![3, 1], false, None)
        );
        // a cursor whose row is gone still bounds the page by id
        assert_eq!(
            feed(&app, &format!("{base}&before_id=5")).await.0,
            vec![4, 3, 1]
        );
        assert_eq!(
            feed(&app, &format!("{base}&before_id=999")).await.0,
            vec![6, 4, 3, 1]
        );
        assert_eq!(
            feed(
                &app,
                &format!("/v1/brc20/wallet/balance_history?address={BOB}&ticker=ordi")
            )
            .await
            .0,
            vec![5, 2]
        );
        expect(
            &app,
            &format!("{base}&limit=1"),
            200,
            JSON,
            &ok(&format!(r#"{{"address":"{ALICE}","pkscript":null,"ticker":"ordi","decimals":18,"ticker_decimals":18,"balances":[{{"id":6,"block_height":301,"block_time":null,"overall_balance":"350","available_balance":"350"}}],"limit":1,"has_more":true,"next_before_id":6,"block_height":301}}"#)),
        )
        .await;
        expect(
            &app,
            &format!("/v1/brc20/wallet/balance_history?pkscript={P_BOB}&ticker=ordi&before_id=5"),
            200,
            JSON,
            &ok(&format!(r#"{{"address":null,"pkscript":"{P_BOB}","ticker":"ordi","decimals":18,"ticker_decimals":18,"balances":[{{"id":2,"block_height":120,"block_time":1200,"overall_balance":"1000","available_balance":"1000"}}],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}}"#)),
        )
        .await;
        // no rows: another ticker, another network's form of the pkScript,
        // an unmapped EVM address and no address at all
        for uri in [
            format!("/v1/brc20/wallet/balance_history?address={ALICE}&ticker=pepe"),
            format!("/v1/brc20/wallet/balance_history?address={ALICE_MAINNET}&ticker=ordi"),
            format!("/v1/brc20/wallet/balance_history?address={STRANGER_EVM}&ticker=ordi"),
            "/v1/brc20/wallet/balance_history?address=tb1qnobody&ticker=ordi".to_string(),
        ] {
            assert_eq!(feed(&app, &uri).await, (vec![], false, None), "{uri}");
        }
        expect(
            &app,
            &format!("/v1/brc20/wallet/balance_history?address={ALICE}&ticker=nope"),
            404,
            JSON,
            &bad("ticker not found"),
        )
        .await;
    })
    .await;
}

/// The batch body of `ids`.
fn ids_body(ids: &[String]) -> String {
    json!({ "inscription_ids": ids }).to_string()
}

/// The inscription id whose txid is `byte` 32 times, with index `index`.
fn inscription(byte: &str, index: u32) -> String {
    format!("{}i{index}", byte.repeat(32))
}

const EVENTS_BY_INSCRIPTIONS: &str = "/v1/brc20/events_by_inscriptions";

#[tokio::test]
async fn events_by_inscriptions_refuses_a_bad_body_before_any_query() {
    let app = height(BRC20, 7.into());
    let shape = "the body must be JSON {\"inscription_ids\":[...]} with 1 to 500 inscription ids";
    let item = "inscription_ids[1] must be an inscription id <txid>i<index> (64 hex digits, `i` and a 32-bit index)";
    let one = inscription("ab", 0);
    for (body, reason) in [
        (String::new(), shape),
        ("{".into(), shape),
        (json!([one]).to_string(), shape),
        (ids_body(&[]), shape),
        (ids_body(&vec![one.clone(); 501]), shape),
        (ids_body(&[one.clone(), "t1i0".into()]), item),
    ] {
        let answer = post(&app, EVENTS_BY_INSCRIPTIONS, &body).await;
        assert_eq!(answer.status, 400, "{body}");
        let refusal = json!({"error": reason, "result": null}).to_string();
        assert_eq!(answer.body, refusal, "{body}");
    }
    let answer = post(&app, EVENTS_BY_INSCRIPTIONS, &ids_body(&[one])).await;
    assert_eq!((answer.status, answer.body.as_str()), (500, INTERNAL_ERROR));
}

/// Events under canonical inscription ids, beside the fixture's: `aa…i0`
/// has two full events and a light one, `bb…i0` a mint stored in both
/// tables (one row after the union) and an event of a type
/// `brc20_event_types` does not list, `cc…i5` none.
const CANONICAL_EVENTS: &str = r#"
INSERT INTO brc20_events (id, event_type, block_height, inscription_id, event) VALUES
  (20, 2, 300, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaai0', '{"tick":"ordi","amount":"1"}'),
  (21, 3, 301, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaai0', '{"tick":"ordi","amount":"1","using_tx_id":"u3"}'),
  (22, 1, 301, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbi0', '{"tick":"pepe","amount":"2"}'),
  (23, 999, 301, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbi0', '{"x":"y"}');
INSERT INTO brc20_light_events (id, event_type, block_height, inscription_id, event) VALUES
  (19, 1, 300, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaai0', '{"tick":"ordi","amount":"3"}'),
  (22, 1, 301, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbi0', '{"tick":"pepe","amount":"2"}');
"#;

/// The `result` of a 200 JSON answer.
fn result_of(answer: &common::Answer) -> Value {
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON));
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["error"], Value::Null);
    body["result"].clone()
}

#[tokio::test]
async fn seeded_events_by_inscriptions_answer_each_id_as_the_single_route() {
    with_seeded("byinsc", |app, pool| async move {
        sqlx::raw_sql(CANONICAL_EVENTS)
            .execute(&pool)
            .await
            .unwrap();
        let (a, b, c) = (
            inscription("aa", 0),
            inscription("bb", 0),
            inscription("cc", 5),
        );
        let requested = [
            format!("{}i00", "BB".repeat(32)),
            c.clone(),
            a.clone(),
            b.clone(),
        ];
        let answer = post(&app, EVENTS_BY_INSCRIPTIONS, &ids_body(&requested)).await;
        let result = result_of(&answer);
        assert_eq!(result["block_height"], 301);
        let entries = result["inscriptions"].as_array().unwrap();
        let listed: Vec<&str> = entries
            .iter()
            .map(|entry| entry["inscription_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed, [&b, &c, &a, &b]);
        for entry in entries {
            let id = entry["inscription_id"].as_str().unwrap();
            let single =
                result_of(&get(&app, &format!("/v1/brc20/event?inscription_id={id}")).await);
            assert_eq!(entry["events"], single, "{id}");
        }
        // the union's order and dedupe, the event type names, an id without
        // events
        let amounts = |entry: &Value| -> Vec<(Value, Value)> {
            entry["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| (event["event_type"].clone(), event["amount"].clone()))
                .collect()
        };
        assert_eq!(
            amounts(&entries[2]),
            [
                (json!("mint-inscribe"), json!("3")),
                (json!("transfer-inscribe"), json!("1")),
                (json!("transfer-transfer"), json!("1")),
            ]
        );
        assert_eq!(
            amounts(&entries[0]),
            [
                (json!("mint-inscribe"), json!("2")),
                (Value::Null, Value::Null)
            ]
        );
        assert!(entries[0]["events"][1].get("event_type").is_none());
        assert_eq!(entries[0]["events"][1]["inscription_id"], json!(b));
        assert_eq!(entries[1]["events"], json!([]));
        // the cap is 500 ids
        let many: Vec<String> = (0..500).map(|n| format!("{n:064x}i{n}")).collect();
        let answer = post(&app, EVENTS_BY_INSCRIPTIONS, &ids_body(&many)).await;
        assert_eq!(
            result_of(&answer)["inscriptions"].as_array().unwrap().len(),
            500
        );
    })
    .await;
}
