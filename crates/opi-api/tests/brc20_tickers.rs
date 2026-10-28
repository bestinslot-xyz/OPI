//! The brc20 ticker routes over HTTP: `/v1/brc20/ticker`, `/v1/brc20/tickers`,
//! `/v1/brc20/ticker/holders` and `/v1/brc20/wallet/balances`.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters are checked before any query, and a valid request
//! answers the 500 body when its query fails. The seeded tests build a
//! throwaway database from opi-brc20's db_init.sql on the local Postgres
//! (127.0.0.1:5432, the current user), fill it with BRC-20 and BRC2.0
//! balances and drop it afterwards; they skip unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{INTERNAL_ERROR, JSON, app, expect, height},
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
    for (uri, reason) in [
        ("/v1/brc20/ticker", "ticker is required"),
        ("/v1/brc20/ticker?ticker=", "ticker is required"),
        (
            "/v1/brc20/ticker?ticker=a&ticker=b",
            "ticker must be a single value",
        ),
        ("/v1/brc20/tickers?limit=0", limit),
        ("/v1/brc20/tickers?limit=501", limit),
        (
            "/v1/brc20/tickers?offset=x",
            "offset must be an integer from 0 to 1000000000",
        ),
        ("/v1/brc20/tickers?order=up", "order must be asc or desc"),
        (
            "/v1/brc20/tickers?sort=price",
            "sort must be one of holders, prog_holders, max_supply, mint_progress, deploy_height",
        ),
        (
            "/v1/brc20/tickers?search[a]=x",
            "search must be a single value",
        ),
        (
            "/v1/brc20/ticker?ticker=a%00",
            "ticker must be at most 256 bytes without NUL",
        ),
        (
            "/v1/brc20/tickers?search=a%00",
            "search must be at most 256 bytes without NUL",
        ),
        (
            "/v1/brc20/ticker/holders?ticker=a%00",
            "ticker must be at most 256 bytes without NUL",
        ),
        (
            "/v1/brc20/wallet/balances?address=a%00",
            "address must be at most 128 bytes without NUL",
        ),
        ("/v1/brc20/ticker/holders", "ticker is required"),
        ("/v1/brc20/ticker/holders?ticker=ordi&limit=1000", limit),
        ("/v1/brc20/wallet/balances", "address is required"),
        ("/v1/brc20/wallet/balances?address=x&limit=-1", limit),
        (
            "/v1/brc20/ticker/by_contract",
            "contract_address is required",
        ),
        (
            "/v1/brc20/ticker/by_contract?contract_address=0x11",
            "contract_address must be an EVM address: 0x and 40 hex digits",
        ),
        (
            "/v1/brc20/ticker/by_contract?contract_address=0xg111111111111111111111111111111111111111",
            "contract_address must be an EVM address: 0x and 40 hex digits",
        ),
        (
            "/v1/brc20/ticker/by_contract?contract_address=a&contract_address=b",
            "contract_address must be a single value",
        ),
    ] {
        expect(&app, uri, 400, JSON, &bad(reason)).await;
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(BRC20, 7.into());
    for uri in [
        "/v1/brc20/ticker?ticker=ordi",
        "/v1/brc20/tickers",
        "/v1/brc20/tickers?sort=mint_progress&order=asc&search=or&offset=5&limit=500",
        "/v1/brc20/ticker/holders?ticker=ordi",
        "/v1/brc20/wallet/balances?address=bc1qxyz",
        "/v1/brc20/wallet/balances?address=0xAbCdEf0123456789aBcDeF0123456789AbCdEf01",
        "/v1/brc20/ticker/by_contract?contract_address=0x1111111111111111111111111111111111111111",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

const ALICE: &str = "tb1qalice";
const ALICE_EVM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAROL_EVM: &str = "0xcccccccccccccccccccccccccccccccccccccccc";
const CONTRACT_EVM: &str = "0xdddddddddddddddddddddddddddddddddddddddd";

/// The BRC2.0 module's pkScript, which holds the BRC-20 side of deposits.
const MODULE: &str = "6a09425243323050524f47";
/// An EVM wallet holding only a ticker brc20 does not list.
const GHOST_EVM: &str = "0xffffffffffffffffffffffffffffffffffffffff";

/// One token in raw units.
const E18: &str = "000000000000000000";

/// The fixture: tickers `ordi` (partly minted), `pepe` (fully minted) and
/// `o_rd` (max supply 0); BRC-20 holders Alice and Bob, a pkScript without an
/// address, a zero balance and the BRC2.0 module's pkScript (never listed);
/// BRC2.0 holders Alice's EVM address, Carol's (who has no BRC-20 balance),
/// an unmapped contract, a zero balance and a wallet of an unlisted ticker.
fn fixture() -> String {
    format!(
        "INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (300, 'h300');
         INSERT INTO brc20_tickers (original_tick, tick, max_supply, decimals, limit_per_mint,
                                    remaining_supply, burned_supply, is_self_mint,
                                    deploy_inscription_id, block_height)
         VALUES ('ORDI', 'ordi', 21000000{E18}, 18, 1000{E18}, 1000{E18}, 0, false, 'ordii0', 100),
                ('pepe', 'pepe', 1000{E18}, 8, 1000{E18}, 0, 5{E18}, false, 'pepei0', 200),
                ('o_rd', 'o_rd', 0, 0, 0, 0, 0, true, 'o_rdi0', 150);
         INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance,
                                             available_balance, block_height)
         VALUES ('0014a1', '{ALICE}', 'ordi', 500, 400, 120),
                ('0014b2', 'tb1qbob', 'ordi', 300, 300, 120),
                ('6a01ff', '', 'ordi', 300, 300, 120),
                ('0014e5', 'tb1qempty', 'ordi', 0, 0, 120),
                ('{MODULE}', '', 'ordi', 550, 550, 260),
                ('0014a1', '{ALICE}', 'pepe', 10, 10, 220);
         INSERT INTO evm_addresses (evm_address, btc_address)
         VALUES ('{ALICE_EVM}', '{ALICE}'),
                ('{CAROL_EVM}', 'tb1qcarol');
         INSERT INTO brc20_prog_tickers (contract_address, ticker, ticker_hash, block_height)
         VALUES ('0x1111111111111111111111111111111111111111', 'ordi', 'hash', 250);
         INSERT INTO brc20_prog_current_balances (wallet, ticker, amount, block_height)
         VALUES ('{ALICE_EVM}', 'ordi', 200, 260),
                ('{CAROL_EVM}', 'ordi', 50, 260),
                ('{CONTRACT_EVM}', 'ordi', 300, 260),
                ('0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', 'ordi', 0, 260),
                ('{GHOST_EVM}', 'ghost', 9, 260);
         INSERT INTO brc20_ticker_holders (tick, holders, prog_holders)
         VALUES ('ordi', 3, 3), ('pepe', 1, 0);"
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
    let name = format!("opi_api_tickers_{tag}_{}_{nanos}", std::process::id());
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

const ORDI: &str = r#"{"tick":"ordi","original_tick":"ORDI","decimals":18,"ticker_decimals":18,"max_supply":"21000000000000000000000000","minted_supply":"20999000000000000000000000","remaining_supply":"1000000000000000000000","burned_supply":"0","limit_per_mint":"1000000000000000000000","is_self_mint":false,"deploy_inscription_id":"ordii0","deploy_block_height":100,"holders":3,"prog_holders":3"#;

#[tokio::test]
async fn seeded_ticker_answers_its_supply_counts_and_contract() {
    with_seeded("ticker", |app| async move {
        let body = ok(&format!(
            r#"{ORDI},"prog_contract_address":"0x1111111111111111111111111111111111111111","block_height":300}}"#
        ));
        expect(&app, "/v1/brc20/ticker?ticker=ordi", 200, JSON, &body).await;
        expect(&app, "/v1/brc20/ticker?ticker=OrDi", 200, JSON, &body).await;
        // A ticker without a rollup row or BRC2.0 contract counts zero.
        expect(
            &app,
            "/v1/brc20/ticker?ticker=o_rd",
            200,
            JSON,
            &ok(r#"{"tick":"o_rd","original_tick":"o_rd","decimals":18,"ticker_decimals":0,"max_supply":"0","minted_supply":"0","remaining_supply":"0","burned_supply":"0","limit_per_mint":"0","is_self_mint":true,"deploy_inscription_id":"o_rdi0","deploy_block_height":150,"holders":0,"prog_holders":0,"prog_contract_address":null,"block_height":300}"#),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker?ticker=nope",
            404,
            JSON,
            &bad("ticker not found"),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_ticker_by_contract_answers_the_wrapped_ticker() {
    with_seeded("by_contract", |app| async move {
        let body = ok(&format!(
            r#"{ORDI},"prog_contract_address":"0x1111111111111111111111111111111111111111","prog_ticker_hash":"hash","block_height":300}}"#
        ));
        expect(
            &app,
            "/v1/brc20/ticker/by_contract?contract_address=0x1111111111111111111111111111111111111111",
            200,
            JSON,
            &body,
        )
        .await;
        // The address matches in lowercase, as the engine logs it.
        expect(
            &app,
            "/v1/brc20/ticker/by_contract?contract_address=0X1111111111111111111111111111111111111111",
            200,
            JSON,
            &body,
        )
        .await;
        expect(
            &app,
            &format!("/v1/brc20/ticker/by_contract?contract_address={CONTRACT_EVM}"),
            404,
            JSON,
            &bad("contract not found"),
        )
        .await;
    })
    .await;
}

/// The ticks of a `/v1/brc20/tickers` answer, in order, and its `has_more`.
async fn ticks(app: &Router, uri: &str) -> (Vec<String>, bool) {
    let answer = common::get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    let ticks = answer
        .body
        .split(r#"{"tick":""#)
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].to_string())
        .collect();
    (ticks, answer.body.contains(r#""has_more":true"#))
}

#[tokio::test]
async fn seeded_tickers_sort_search_and_page() {
    with_seeded("tickers", |app| async move {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (uri, expected) in [
            ("/v1/brc20/tickers", names(&["ordi", "pepe", "o_rd"])),
            (
                "/v1/brc20/tickers?order=asc",
                names(&["o_rd", "pepe", "ordi"]),
            ),
            (
                "/v1/brc20/tickers?sort=prog_holders",
                names(&["ordi", "o_rd", "pepe"]),
            ),
            (
                "/v1/brc20/tickers?sort=max_supply",
                names(&["ordi", "pepe", "o_rd"]),
            ),
            (
                "/v1/brc20/tickers?sort=mint_progress",
                names(&["pepe", "ordi", "o_rd"]),
            ),
            (
                "/v1/brc20/tickers?sort=deploy_height&order=asc",
                names(&["ordi", "o_rd", "pepe"]),
            ),
            ("/v1/brc20/tickers?search=O", names(&["ordi", "o_rd"])),
            // `_` is a literal, not LIKE's any-character
            ("/v1/brc20/tickers?search=o_", names(&["o_rd"])),
            ("/v1/brc20/tickers?search=x", names(&[])),
        ] {
            assert_eq!(ticks(&app, uri).await, (expected, false), "{uri}");
        }
        assert_eq!(
            ticks(&app, "/v1/brc20/tickers?limit=2").await,
            (names(&["ordi", "pepe"]), true)
        );
        assert_eq!(
            ticks(&app, "/v1/brc20/tickers?limit=2&offset=2").await,
            (names(&["o_rd"]), false)
        );
        expect(
            &app,
            "/v1/brc20/tickers?search=ord&limit=1",
            200,
            JSON,
            &ok(&format!(
                r#"{{"sort":"holders","order":"desc","tickers":[{ORDI}}}],"offset":0,"limit":1,"has_more":false,"block_height":300}}"#
            )),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_holders_merge_brc20_and_brc20_prog_per_address() {
    with_seeded("holders", |app| async move {
        let alice = format!(
            r#"{{"address":"{ALICE}","pkscript":"0014a1","evm_address":"{ALICE_EVM}","balance":"500","available_balance":"400","prog_balance":"200","total_balance":"700"}}"#
        );
        let contract = format!(
            r#"{{"address":null,"pkscript":null,"evm_address":"{CONTRACT_EVM}","balance":"0","available_balance":"0","prog_balance":"300","total_balance":"300"}}"#
        );
        let no_address = r#"{"address":null,"pkscript":"6a01ff","evm_address":null,"balance":"300","available_balance":"300","prog_balance":"0","total_balance":"300"}"#;
        let bob = r#"{"address":"tb1qbob","pkscript":"0014b2","evm_address":null,"balance":"300","available_balance":"300","prog_balance":"0","total_balance":"300"}"#;
        let carol = format!(
            r#"{{"address":"tb1qcarol","pkscript":null,"evm_address":"{CAROL_EVM}","balance":"0","available_balance":"0","prog_balance":"50","total_balance":"50"}}"#
        );
        let page = |holders: &str, offset: u32, limit: u32, has_more: bool| {
            ok(&format!(
                r#"{{"ticker":"ordi","decimals":18,"ticker_decimals":18,"holders":[{holders}],"offset":{offset},"limit":{limit},"has_more":{has_more},"block_height":300}}"#
            ))
        };
        // The module's pkScript (550, the largest BRC-20 balance) is not
        // listed.
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=ORDI",
            200,
            JSON,
            &page(
                &[alice.as_str(), &contract, no_address, bob, &carol].join(","),
                0,
                100,
                false,
            ),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=ordi&limit=2",
            200,
            JSON,
            &page(&[alice.as_str(), &contract].join(","), 0, 2, true),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=ordi&limit=2&offset=2",
            200,
            JSON,
            &page(&[no_address, bob].join(","), 2, 2, true),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=ordi&limit=2&offset=4",
            200,
            JSON,
            &page(&carol, 4, 2, false),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=o_rd",
            200,
            JSON,
            &ok(r#"{"ticker":"o_rd","decimals":18,"ticker_decimals":0,"holders":[],"offset":0,"limit":100,"has_more":false,"block_height":300}"#),
        )
        .await;
        expect(
            &app,
            "/v1/brc20/ticker/holders?ticker=nope",
            404,
            JSON,
            &bad("ticker not found"),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_wallet_balances_list_both_kinds_by_ticker() {
    with_seeded("wallet", |app| async move {
        let alice = |offset: u32, limit: u32, balances: &str, has_more: bool| {
            ok(&format!(
                r#"{{"address":"{ALICE}","evm_address":"{ALICE_EVM}","balances":[{balances}],"offset":{offset},"limit":{limit},"has_more":{has_more},"block_height":300}}"#
            ))
        };
        let ordi = r#"{"tick":"ordi","decimals":18,"ticker_decimals":18,"balance":"500","available_balance":"400","prog_balance":"200","total_balance":"700"}"#;
        let pepe = r#"{"tick":"pepe","decimals":18,"ticker_decimals":8,"balance":"10","available_balance":"10","prog_balance":"0","total_balance":"10"}"#;
        let both = format!("{ordi},{pepe}");
        expect(
            &app,
            &format!("/v1/brc20/wallet/balances?address={ALICE}"),
            200,
            JSON,
            &alice(0, 100, &both, false),
        )
        .await;
        // By EVM address, in any case.
        expect(
            &app,
            &format!(
                "/v1/brc20/wallet/balances?address={}",
                ALICE_EVM.replace("0xaa", "0xAA")
            ),
            200,
            JSON,
            &alice(0, 100, &both, false),
        )
        .await;
        expect(
            &app,
            &format!("/v1/brc20/wallet/balances?address={ALICE}&limit=1"),
            200,
            JSON,
            &alice(0, 1, ordi, true),
        )
        .await;
        expect(
            &app,
            &format!("/v1/brc20/wallet/balances?address={ALICE}&limit=1&offset=1"),
            200,
            JSON,
            &alice(1, 1, pepe, false),
        )
        .await;
        // An EVM address without a Bitcoin address.
        expect(
            &app,
            &format!("/v1/brc20/wallet/balances?address={CONTRACT_EVM}"),
            200,
            JSON,
            &ok(&format!(
                r#"{{"address":null,"evm_address":"{CONTRACT_EVM}","balances":[{{"tick":"ordi","decimals":18,"ticker_decimals":18,"balance":"0","available_balance":"0","prog_balance":"300","total_balance":"300"}}],"offset":0,"limit":100,"has_more":false,"block_height":300}}"#
            )),
        )
        .await;
        // A Bitcoin address with only a BRC2.0 balance.
        expect(
            &app,
            "/v1/brc20/wallet/balances?address=tb1qcarol",
            200,
            JSON,
            &ok(&format!(
                r#"{{"address":"tb1qcarol","evm_address":"{CAROL_EVM}","balances":[{{"tick":"ordi","decimals":18,"ticker_decimals":18,"balance":"0","available_balance":"0","prog_balance":"50","total_balance":"50"}}],"offset":0,"limit":100,"has_more":false,"block_height":300}}"#
            )),
        )
        .await;
        // A ticker brc20 does not list has no deploy precision.
        expect(
            &app,
            &format!("/v1/brc20/wallet/balances?address={GHOST_EVM}"),
            200,
            JSON,
            &ok(&format!(
                r#"{{"address":null,"evm_address":"{GHOST_EVM}","balances":[{{"tick":"ghost","decimals":18,"ticker_decimals":null,"balance":"0","available_balance":"0","prog_balance":"9","total_balance":"9"}}],"offset":0,"limit":100,"has_more":false,"block_height":300}}"#
            )),
        )
        .await;
        // Zero balances are left out, and an unknown address holds nothing.
        for address in ["tb1qempty", "tb1qnobody"] {
            expect(
                &app,
                &format!("/v1/brc20/wallet/balances?address={address}"),
                200,
                JSON,
                &ok(&format!(
                    r#"{{"address":"{address}","evm_address":null,"balances":[],"offset":0,"limit":100,"has_more":false,"block_height":300}}"#
                )),
            )
            .await;
        }
    })
    .await;
}
