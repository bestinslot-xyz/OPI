//! The brc20 event feeds over HTTP: `/v1/brc20/ticker/events` and
//! `/v1/brc20/wallet/events`.
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
    common::{INTERNAL_ERROR, JSON, app, expect, get, height},
    opi_api::{config::Module, db::Db},
    sqlx::{
        Connection, PgConnection,
        postgres::{PgConnectOptions, PgPoolOptions},
    },
};

const BRC20: Module = Module::Brc20;

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

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
        ("/v1/brc20/wallet/events", "address or pkscript is required"),
        (
            "/v1/brc20/wallet/events?address=a&pkscript=00",
            "give address or pkscript, not both",
        ),
        (
            "/v1/brc20/wallet/events?pkscript=xyz",
            "pkscript must be hex of at most 20000 digits",
        ),
        ("/v1/brc20/wallet/events?address=a&limit=1000", limit),
        ("/v1/brc20/wallet/events?address=a&before_id=-5", before),
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
        "/v1/brc20/wallet/events?address=bc1qxyz",
        "/v1/brc20/wallet/events?pkscript=0014a1",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

const ALICE: &str = "tb1qalice";
const BOB: &str = "tb1qbob";
const CAROL: &str = "tb1qcarol";
const P_ALICE: &str = "0014a1";
const P_BOB: &str = "0014b2";
const P_CAROL: &str = "0014c3";
/// The BRC2.0 module's pkScript, the spent party of a deposit.
const MODULE: &str = "6a09425243323050524f47";
const ALICE_EVM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const STRANGER_EVM: &str = "0x9999999999999999999999999999999999999999";

/// One token in raw units.
const E18: &str = "000000000000000000";

/// The fixture. Events (ids ascend with the block height):
/// 1 predeploy (Alice), 2 deploy of ordi (Alice), 3 mint of ordi (Bob),
/// 4 mint of ordi (Alice), 5 transfer inscription of ordi (Alice),
/// 6 transfer of ordi from Alice to Bob, 7 mint of pepe (Bob), 8 deposit of
/// ordi into BRC2.0 (a transfer from Alice to the module's pkScript, above
/// the journal tip: no block time), 9 BRC2.0 call (Alice, no ticker); and
/// light event 1001, a mint of ordi (Carol) in a block with no hash row. A
/// real database fills one of the two event tables, so the light id
/// continues the other's.
fn fixture() -> String {
    format!(
        "INSERT INTO brc20_block_hashes (block_height, block_hash, block_time) VALUES
            (90, 'h90', 900), (100, 'h100', 1000), (120, 'h120', 1200),
            (200, 'h200', 2000), (300, 'h300', 3000), (301, 'h301', NULL);
         INSERT INTO brc20_tickers (original_tick, tick, max_supply, decimals, limit_per_mint,
                                    remaining_supply, burned_supply, is_self_mint,
                                    deploy_inscription_id, block_height)
         VALUES ('ORDI', 'ordi', 21000000{E18}, 18, 1000{E18}, 1000{E18}, 0, false, 'ordii0', 100),
                ('pepe', 'pepe', 1000{E18}, 8, 1000{E18}, 0, 0, false, 'pepei0', 200);
         INSERT INTO evm_addresses (evm_address, btc_address) VALUES ('{ALICE_EVM}', '{ALICE}');
         INSERT INTO brc20_events (id, event_type, block_height, inscription_id, inscription_number,
                                   old_satpoint, new_satpoint, txid, event)
         VALUES
          (1, 12, 90, 'pre0', 1, NULL, 'sp', 'tx', '{{\"predeployer_pkScript\":\"{P_ALICE}\",\"predeployer_wallet\":\"{ALICE}\",\"hash\":\"hh\",\"block_height\":\"95\"}}'),
          (2, 0, 100, 'ordii0', 2, NULL, 'sp', 'tx', '{{\"deployer_pkScript\":\"{P_ALICE}\",\"deployer_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"decimals\":\"18\",\"max_supply\":\"21000000{E18}\",\"limit_per_mint\":\"1000{E18}\",\"is_self_mint\":\"false\"}}'),
          (3, 1, 120, 'm1i0', 3, NULL, 'sp', 'tx', '{{\"minted_pkScript\":\"{P_BOB}\",\"minted_wallet\":\"{BOB}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"1000{E18}\",\"parent_id\":\"\"}}'),
          (4, 1, 120, 'm2i0', 4, NULL, 'sp', 'tx', '{{\"minted_pkScript\":\"{P_ALICE}\",\"minted_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"500{E18}\",\"parent_id\":\"\"}}'),
          (5, 2, 200, 't1i0', 5, NULL, 'sp', 'tx', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"100{E18}\"}}'),
          (6, 3, 200, 't1i0', 6, 'sp0', 'sp', 'tx', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"spent_pkScript\":\"{P_BOB}\",\"spent_wallet\":\"{BOB}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"100{E18}\",\"using_tx_id\":\"u\"}}'),
          (7, 1, 300, 'm3i0', 7, NULL, 'sp', 'tx', '{{\"minted_pkScript\":\"{P_BOB}\",\"minted_wallet\":\"{BOB}\",\"tick\":\"pepe\",\"original_tick\":\"pepe\",\"amount\":\"7{E18}\",\"parent_id\":\"\"}}'),
          (8, 3, 301, 't2i0', 8, 'sp0', 'sp', 'tx', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"spent_pkScript\":\"{MODULE}\",\"spent_wallet\":null,\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"50{E18}\",\"using_tx_id\":\"u2\"}}'),
          (9, 6, 301, 'c1i0', 9, NULL, 'sp', 'tx', '{{\"source_pkScript\":\"{P_ALICE}\",\"source_wallet\":\"{ALICE}\",\"contract_address\":\"0x1111111111111111111111111111111111111111\",\"data\":null}}');
         INSERT INTO brc20_light_events (id, event_type, block_height, inscription_id, event)
         VALUES (1001, 1, 310, 'm4i0', '{{\"minted_pkScript\":\"{P_CAROL}\",\"minted_wallet\":\"{CAROL}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"9{E18}\",\"parent_id\":\"\"}}');"
    )
}

/// Runs `body` with the app on a throwaway database seeded with
/// [`fixture`], dropped even when `body` panics.
async fn with_seeded<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Router) -> Fut,
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
    let result = tokio::spawn(body(router)).await;
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

/// A feed answer's event ids in order, its `has_more` and `next_before_id`.
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
    with_seeded("ticker", |app| async move {
        // the light event 1001 leads the full table's ordi events
        let all = vec![1001, 8, 6, 5, 4, 3, 2];
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
            (vec![1001, 8], true, Some(8))
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&limit=3&before_id=8").await,
            (vec![6, 5, 4], true, Some(4))
        );
        assert_eq!(
            feed(&app, "/v1/brc20/ticker/events?ticker=ordi&before_id=4").await,
            (vec![3, 2], false, None)
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
            &ok(r#"{"ticker":"pepe","decimals":18,"ticker_decimals":8,"event_type":null,"events":[{"id":7,"block_height":300,"block_time":3000,"inscription_id":"m3i0","event_type":"mint-inscribe","ticker_decimals":8,"event":{"tick":"pepe","amount":"7000000000000000000","parent_id":"","minted_wallet":"tb1qbob","original_tick":"pepe","minted_pkScript":"0014b2"}}],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}"#),
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
async fn seeded_events_carry_block_time_or_null() {
    with_seeded("time", |app| async move {
        // 8 is above the journal tip (NULL time), 1001 is in a block with no
        // hash row
        let answer = get(&app, "/v1/brc20/ticker/events?ticker=ordi").await;
        let times: Vec<&str> = answer
            .body
            .split(r#""block_time":"#)
            .skip(1)
            .map(|rest| &rest[..rest.find(',').unwrap()])
            .collect();
        assert_eq!(times, ["null", "null", "2000", "2000", "1200", "1200", "1000"]);
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
    with_seeded("wallet", |app| async move {
        // Alice: predeploy, her mint, the transfer inscription, the transfer,
        // the deposit and the call (the deploy is hers too)
        let alice = vec![9, 8, 6, 5, 4, 2, 1];
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
            feed(&app, "/v1/brc20/wallet/events?pkscript=0014A1").await.0,
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
        // Bob appears as a minter, and as the spent party of Alice's transfer
        assert_eq!(
            feed(&app, &format!("/v1/brc20/wallet/events?address={BOB}")).await.0,
            vec![7, 6, 3]
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
        // an EVM address with no Bitcoin address, and a wallet with no events
        expect(
            &app,
            &format!("/v1/brc20/wallet/events?address={STRANGER_EVM}"),
            200,
            JSON,
            &ok(r#"{"address":null,"pkscript":null,"decimals":18,"event_type":null,"events":[],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}"#),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/wallet/events?address=tb1qnobody",
            200,
            JSON,
            &ok(r#"{"address":"tb1qnobody","pkscript":null,"decimals":18,"event_type":null,"events":[],"limit":100,"has_more":false,"next_before_id":null,"block_height":301}"#),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_events_page_and_filter() {
    with_seeded("walletpage", |app| async move {
        let base = format!("/v1/brc20/wallet/events?address={ALICE}");
        assert_eq!(
            feed(&app, &format!("{base}&limit=3")).await,
            (vec![9, 8, 6], true, Some(6))
        );
        assert_eq!(
            feed(&app, &format!("{base}&limit=3&before_id=6")).await,
            (vec![5, 4, 2], true, Some(2))
        );
        assert_eq!(
            feed(&app, &format!("{base}&limit=3&before_id=2")).await,
            (vec![1], false, None)
        );
        assert_eq!(
            feed(&app, &format!("{base}&type=transfer-transfer")).await,
            (vec![8, 6], false, None)
        );
        assert_eq!(
            feed(&app, &format!("{base}&type=brc20prog-call-inscribe")).await,
            (vec![9], false, None)
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
