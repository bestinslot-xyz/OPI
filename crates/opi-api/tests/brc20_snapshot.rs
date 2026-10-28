//! `/v1/brc20/ticker/holders/snapshot` over HTTP.
//!
//! The first tests use a database double that answers only the latest block
//! height. The seeded tests build a throwaway database from opi-brc20's
//! db_init.sql on the local Postgres (127.0.0.1:5432, the current user), fill
//! it with a BRC-20 and BRC2.0 balance history and current balances that
//! follow from it, and drop it afterwards; they skip unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::Request,
        response::Response,
    },
    common::{INTERNAL_ERROR, JSON, expect, height, with_seeded_pool},
    http_body_util::BodyExt,
    opi_api::{config::Module, db::Db},
    sqlx::PgPool,
    std::net::SocketAddr,
    tower::ServiceExt,
};

const BRC20: Module = Module::Brc20;
const CSV: &str = "text/csv; charset=utf-8";
const ROUTE: &str = "/v1/brc20/ticker/holders/snapshot";

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(BRC20, 300.into());
    let height = "block_height must be an integer from 0 to 2147483647";
    let long = format!("?ticker={}", "a".repeat(257));
    for (query, reason) in [
        ("", "ticker is required"),
        (
            "?ticker=a%00",
            "ticker must be at most 256 bytes without NUL",
        ),
        (
            long.as_str(),
            "ticker must be at most 256 bytes without NUL",
        ),
        ("?ticker=ordi&block_height=-1", height),
        ("?ticker=ordi&block_height=x", height),
        ("?ticker=ordi&block_height=2147483648", height),
        ("?ticker=ordi&format=xml", "format must be json or csv"),
    ] {
        expect(&app, &format!("{ROUTE}{query}"), 400, JSON, &bad(reason)).await;
    }
    // A valid request answers 500 when its query fails.
    expect(
        &app,
        &format!("{ROUTE}?ticker=ordi&block_height=5"),
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}

const ALICE_EVM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAROL_EVM: &str = "0xcccccccccccccccccccccccccccccccccccccccc";
const CONTRACT_EVM: &str = "0xdddddddddddddddddddddddddddddddddddddddd";
const MODULE: &str = "6a09425243323050524f47";

/// The fixture, tip 300. `ordi` (deployed at 100) has this BRC-20 history:
/// Alice 100 at 110, 200 then 500/400 at 120 (two rows of one block), 900/800
/// at 250; Bob 300 at 110 and 0 at 260; Dave 0 at 120 and 40 at 270; the
/// pkScript `6a01ff` without an address 300 at 120; a wallet whose name
/// needs CSV quoting 70 at 140; the module pkScript 550 at 130 and 800 at
/// 280 (never listed). Its BRC2.0 history: Alice's EVM address 200 at 250
/// and 150 at 280; Carol's (mapped, no BRC-20 balance) 50 at 255; an
/// unmapped contract 300 at 150 and 0 at 290; a zero balance at 260. `big`
/// has 3000 holders, enough for many body chunks, `huge` 20000, more than
/// the channel buffers, and `a"b` none. The current balances are each
/// holder's latest row, as the indexer keeps them.
fn fixture() -> String {
    format!(
        r#"INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (300, 'h300');
         INSERT INTO brc20_tickers (original_tick, tick, max_supply, decimals, limit_per_mint,
                                    remaining_supply, burned_supply, is_self_mint,
                                    deploy_inscription_id, block_height)
         VALUES ('ORDI', 'ordi', 1000000, 18, 1000, 0, 0, false, 'ordii0', 100),
                ('pepe', 'pepe', 1000, 8, 1000, 0, 0, false, 'pepei0', 200),
                ('big', 'big', 1000000, 18, 1000, 0, 0, false, 'bigi0', 100),
                ('huge', 'huge', 1000000, 18, 1000, 0, 0, false, 'hugei0', 100),
                ('a"b', 'a"b', 1000, 0, 1000, 1000, 0, false, 'abi0', 100);
         INSERT INTO brc20_historic_balances (pkscript, wallet, tick, overall_balance,
                                              available_balance, block_height, event_id)
         VALUES ('0014a1', 'tb1qalice', 'ordi', 100, 100, 110, 1),
                ('0014b2', 'tb1qbob', 'ordi', 300, 300, 110, 2),
                ('0014a1', 'tb1qalice', 'ordi', 200, 200, 120, 3),
                ('0014d4', 'tb1qdave', 'ordi', 0, 0, 120, 4),
                ('0014a1', 'tb1qalice', 'ordi', 500, 400, 120, 5),
                ('6a01ff', '', 'ordi', 300, 300, 120, 6),
                ('{MODULE}', '', 'ordi', 550, 550, 130, 7),
                ('0014f6', 'tb1q"odd,name', 'ordi', 70, 70, 140, 8),
                ('0014a1', 'tb1qalice', 'pepe', 10, 10, 220, 9),
                ('0014a1', 'tb1qalice', 'ordi', 900, 800, 250, 10),
                ('0014b2', 'tb1qbob', 'ordi', 0, 0, 260, 11),
                ('0014d4', 'tb1qdave', 'ordi', 40, 40, 270, 12),
                ('{MODULE}', '', 'ordi', 800, 800, 280, 13);
         INSERT INTO brc20_historic_balances (pkscript, wallet, tick, overall_balance,
                                              available_balance, block_height, event_id)
         SELECT '00' || g, 'tb1qbig' || g, 'big', g % 700, g % 300, 100 + g % 150, 1000 + g
         FROM generate_series(1, 3000) g;
         INSERT INTO brc20_historic_balances (pkscript, wallet, tick, overall_balance,
                                              available_balance, block_height, event_id)
         SELECT '01' || g, 'tb1qhuge' || g, 'huge', g, g, 200, 10000 + g
         FROM generate_series(1, 20000) g;
         INSERT INTO evm_addresses (evm_address, btc_address)
         VALUES ('{ALICE_EVM}', 'tb1qalice'), ('{CAROL_EVM}', 'tb1qcarol');
         INSERT INTO brc20_prog_tickers (contract_address, ticker, ticker_hash, block_height)
         VALUES ('0x1111111111111111111111111111111111111111', 'ordi', 'hash', 150);
         INSERT INTO brc20_prog_historical_balances (wallet, ticker, amount, block_height)
         VALUES ('{ALICE_EVM}', 'ordi', 200, 250),
                ('{ALICE_EVM}', 'ordi', 150, 280),
                ('{CAROL_EVM}', 'ordi', 50, 255),
                ('{CONTRACT_EVM}', 'ordi', 300, 150),
                ('{CONTRACT_EVM}', 'ordi', 0, 290),
                ('0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', 'ordi', 0, 260),
                ('0xffffffffffffffffffffffffffffffffffffffff', 'ghost', 9, 260);
         INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance,
                                             available_balance, block_height)
         SELECT DISTINCT ON (pkscript, tick) pkscript, wallet, tick, overall_balance,
                available_balance, block_height
         FROM brc20_historic_balances ORDER BY pkscript, tick, id DESC;
         INSERT INTO brc20_prog_current_balances (wallet, ticker, amount, block_height)
         SELECT DISTINCT ON (wallet, ticker) wallet, ticker, amount, block_height
         FROM brc20_prog_historical_balances ORDER BY wallet, ticker, block_height DESC;"#
    )
}

const DB_INIT: &str = include_str!("../../opi-brc20/src/database/sql/db_init.sql");

/// Runs `body` with the app and its pool of `max_connections` on a
/// throwaway database seeded with [`fixture`].
async fn with_history_pool<F, Fut>(tag: &str, max_connections: u32, body: F)
where
    F: FnOnce(Router, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let fixture = fixture();
    with_seeded_pool(
        BRC20,
        &format!("snapshot_{tag}"),
        None,
        &[DB_INIT, &fixture],
        max_connections,
        body,
    )
    .await;
}

/// [`with_history_pool`] with 10 connections, so the cap is 4.
async fn with_history<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Router) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_history_pool(tag, 10, |app, _| body(app)).await;
}

/// The pool's connections in use.
fn in_use(pool: &PgPool) -> usize {
    pool.size() as usize - pool.num_idle()
}

/// Waits up to 10 s until no pool connection is in use.
async fn until_idle(pool: &PgPool) {
    until_idle_within(pool, std::time::Duration::from_secs(10)).await;
}

/// Waits up to `wait` until no pool connection is in use.
async fn until_idle_within(pool: &PgPool, wait: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    while in_use(pool) > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} connections still in use",
            in_use(pool)
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn ok(result: &str) -> String {
    format!(r#"{{"error":null,"result":{result}}}"#)
}

fn holder(
    address: Option<&str>,
    pkscript: Option<&str>,
    evm: Option<&str>,
    balances: [u32; 3],
) -> String {
    let text = |value: Option<&str>| value.map_or("null".to_string(), common::js_string);
    let [balance, available, prog] = balances;
    format!(
        r#"{{"address":{},"pkscript":{},"evm_address":{},"balance":"{balance}","available_balance":"{available}","prog_balance":"{prog}","total_balance":"{}"}}"#,
        text(address),
        text(pkscript),
        text(evm),
        balance + prog
    )
}

fn snapshot(height: u32, ticker_decimals: u32, holders: &[String]) -> String {
    ok(&format!(
        r#"{{"ticker":"ordi","block_height":{height},"decimals":18,"ticker_decimals":{ticker_decimals},"holders":[{}]}}"#,
        holders.join(",")
    ))
}

/// The `holders` array of a JSON answer.
fn holders_of(body: &str) -> &str {
    let start = body.find(r#""holders":["#).unwrap() + r#""holders":["#.len();
    let end = body[start..].find("]").unwrap() + start;
    &body[start..end]
}

#[tokio::test]
async fn a_past_snapshot_takes_each_holders_latest_row_at_or_below_the_height() {
    with_history("past", |app| async move {
        let odd = holder(Some("tb1q\"odd,name"), Some("0014f6"), None, [70, 70, 0]);
        let contract = holder(None, None, Some(CONTRACT_EVM), [0, 0, 300]);
        let no_address = holder(None, Some("6a01ff"), None, [300, 300, 0]);
        let bob = holder(Some("tb1qbob"), Some("0014b2"), None, [300, 300, 0]);
        // At 200: Alice's later of two rows at 120, the contract's BRC2.0
        // balance under its EVM address, Dave's zero and the module left out.
        let at_200 = snapshot(
            200,
            18,
            &[
                holder(Some("tb1qalice"), Some("0014a1"), None, [500, 400, 0]),
                contract.clone(),
                no_address.clone(),
                bob.clone(),
                odd.clone(),
            ],
        );
        expect(
            &app,
            &format!("{ROUTE}?ticker=ORDI&block_height=200"),
            200,
            JSON,
            &at_200,
        )
        .await;
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi&block_height=200&format=json"),
            200,
            JSON,
            &at_200,
        )
        .await;
        // At 255: Alice's BRC2.0 balance merges into her row through
        // evm_addresses, and Carol is listed by her mapped address.
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi&block_height=255"),
            200,
            JSON,
            &snapshot(
                255,
                18,
                &[
                    holder(
                        Some("tb1qalice"),
                        Some("0014a1"),
                        Some(ALICE_EVM),
                        [900, 800, 200],
                    ),
                    contract,
                    no_address.clone(),
                    bob,
                    odd.clone(),
                    holder(Some("tb1qcarol"), None, Some(CAROL_EVM), [0, 0, 50]),
                ],
            ),
        )
        .await;
        // At the tip: Bob and the contract are at zero.
        let at_tip = snapshot(
            300,
            18,
            &[
                holder(
                    Some("tb1qalice"),
                    Some("0014a1"),
                    Some(ALICE_EVM),
                    [900, 800, 150],
                ),
                no_address,
                odd,
                holder(Some("tb1qcarol"), None, Some(CAROL_EVM), [0, 0, 50]),
                holder(Some("tb1qdave"), Some("0014d4"), None, [40, 40, 0]),
            ],
        );
        expect(&app, &format!("{ROUTE}?ticker=ordi"), 200, JSON, &at_tip).await;
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi&block_height=300"),
            200,
            JSON,
            &at_tip,
        )
        .await;
        // The deploy height itself is a valid, empty snapshot.
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi&block_height=100"),
            200,
            JSON,
            &snapshot(100, 18, &[]),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn the_tip_snapshot_lists_the_holders_routes_rows() {
    with_history("tip", |app| async move {
        for tick in ["ordi", "big", "pepe"] {
            let mut listed = Vec::new();
            for offset in (0..).step_by(500) {
                let page = common::get(
                    &app,
                    &format!("/v1/brc20/ticker/holders?ticker={tick}&limit=500&offset={offset}"),
                )
                .await;
                assert_eq!(page.status, 200, "{}", page.body);
                let rows = holders_of(&page.body);
                if !rows.is_empty() {
                    listed.push(rows.to_string());
                }
                if !page.body.contains(r#""has_more":true"#) {
                    break;
                }
            }
            let snapshot = common::get(&app, &format!("{ROUTE}?ticker={tick}")).await;
            assert_eq!(snapshot.status, 200, "{}", snapshot.body);
            assert_eq!(holders_of(&snapshot.body), listed.join(","), "{tick}");
        }
        // `big` streams in many chunks.
        let big = common::get(&app, &format!("{ROUTE}?ticker=big")).await;
        assert!(big.body.len() > 4 * 64 * 1024, "{}", big.body.len());
        assert_eq!(big.body.matches(r#"{"address":"#).count(), 2996);
    })
    .await;
}

#[tokio::test]
async fn csv_has_a_header_quoted_fields_and_an_attachment_name() {
    with_history("csv", |app| async move {
        let response = send(&app, &format!("{ROUTE}?ticker=ordi&block_height=200&format=csv")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers().get("content-type").unwrap(), CSV);
        assert_eq!(
            response.headers().get("content-disposition").unwrap(),
            "attachment; filename=\"ordi-200.csv\""
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!(
                "address,evm_address,pkscript,brc20_balance,brc20_available_balance,prog_balance,total_balance\r\n\
                 tb1qalice,,0014a1,500,400,0,500\r\n\
                 ,{CONTRACT_EVM},,0,0,300,300\r\n\
                 ,,6a01ff,300,300,0,300\r\n\
                 tb1qbob,,0014b2,300,300,0,300\r\n\
                 \"tb1q\"\"odd,name\",,0014f6,70,70,0,70\r\n"
            )
        );
        // A ticker with a quote: an empty snapshot under a safe file name.
        let response = send(&app, &format!("{ROUTE}?ticker=A%22B&format=csv")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-disposition").unwrap(),
            "attachment; filename=\"a_b-300.csv\"; filename*=UTF-8''a%22b-300.csv"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            &body[..],
            b"address,evm_address,pkscript,brc20_balance,brc20_available_balance,prog_balance,total_balance\r\n"
        );
        expect(
            &app,
            &format!("{ROUTE}?ticker=A%22B"),
            200,
            JSON,
            &ok(r#"{"ticker":"a\"b","block_height":300,"decimals":18,"ticker_decimals":0,"holders":[]}"#),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn heights_out_of_range_and_unknown_tickers_are_refused() {
    with_history_pool("refused", 10, |app, pool| async move {
        for (query, status, reason) in [
            ("ticker=nope", 404, "ticker not found"),
            ("ticker=nope&block_height=200", 404, "ticker not found"),
            (
                "ticker=ordi&block_height=301",
                400,
                "block_height is above the indexed tip",
            ),
            (
                "ticker=ordi&block_height=99",
                400,
                "block_height is below the ticker's deploy height",
            ),
            (
                "ticker=pepe&block_height=199&format=csv",
                400,
                "block_height is below the ticker's deploy height",
            ),
        ] {
            expect(
                &app,
                &format!("{ROUTE}?{query}"),
                status,
                JSON,
                &bad(reason),
            )
            .await;
        }
        // A database with no indexed block has no height to answer.
        sqlx::query("DELETE FROM brc20_block_hashes")
            .execute(&pool)
            .await
            .unwrap();
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi"),
            400,
            JSON,
            &bad("block_height is above the indexed tip"),
        )
        .await;
        // Each refusal returns its connection to the pool.
        until_idle(&pool).await;
    })
    .await;
}

/// Reads `body` to its end: whether it ended with an error, and the bytes
/// it held.
async fn read_to_end(mut body: Body) -> (bool, usize) {
    let mut received = 0;
    loop {
        match body.frame().await {
            Some(Ok(frame)) => received += frame.into_data().map_or(0, |d| d.len()),
            Some(Err(_)) => return (true, received),
            None => return (false, received),
        }
    }
}

#[tokio::test]
async fn a_failure_after_the_first_chunk_ends_the_body_early() {
    with_history_pool("cut", 10, |app, pool| async move {
        let response = send(&app, &format!("{ROUTE}?ticker=huge&format=csv")).await;
        assert_eq!(response.status(), 200);
        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert!(first.starts_with(b"address,"));
        // The snapshot's connection goes away mid-stream.
        sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname = current_database() AND pid <> pg_backend_pid()",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (failed, received) = read_to_end(body).await;
        assert!(failed, "the body ended cleanly after {received} bytes");
    })
    .await;
}

#[tokio::test]
async fn a_snapshot_stops_at_its_time_limit() {
    // A pool of 2 caps snapshots at 1.
    with_history_pool("timeout", 2, |unlimited, pool| async move {
        let uri = format!("{ROUTE}?ticker=huge");
        // The first chunk waits for the whole sort. The limit is 5 s or ten
        // times this machine's time to the first chunk, whichever is
        // longer, so a loaded machine still sees the first chunk in time.
        let started = std::time::Instant::now();
        let probe = send(&unlimited, &uri).await;
        let first_chunk = started.elapsed();
        assert_eq!(probe.status(), 200);
        drop(probe);
        until_idle(&pool).await;
        let limit = (first_chunk * 10).max(std::time::Duration::from_secs(5));
        let app = common::app_with_env(
            BRC20,
            Db::from_pool(pool.clone()),
            &[("API_SNAPSHOT_TIMEOUT_MS", &limit.as_millis().to_string())],
        );
        // A client that never reads.
        let stalled = send(&app, &uri).await;
        assert_eq!(
            stalled.status(),
            200,
            "no first chunk within {limit:?} (the probe took {first_chunk:?})"
        );
        let busy = bad("too many snapshots running, retry later");
        expect(&app, &uri, 503, JSON, &busy).await;
        assert_eq!(in_use(&pool), 1);
        // At the limit the connection closes and the slot frees.
        until_idle_within(&pool, limit + std::time::Duration::from_secs(10)).await;
        let after = common::get(&app, &format!("{ROUTE}?ticker=ordi")).await;
        assert_eq!(after.status, 200, "{}", after.body);
        // The stalled body ends with an error, not a clean truncated end.
        let (failed, received) = read_to_end(stalled.into_body()).await;
        assert!(failed, "the body ended cleanly after {received} bytes");
    })
    .await;
}

#[tokio::test]
async fn a_snapshot_whose_checks_pass_the_limit_answers_503() {
    with_history_pool("checks", 4, |_, pool| async move {
        let app = common::app_with_env(
            BRC20,
            Db::from_pool(pool.clone()),
            &[("API_SNAPSHOT_TIMEOUT_MS", "300")],
        );
        // The tip query waits on this lock past the limit.
        let mut lock = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE brc20_block_hashes IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *lock)
            .await
            .unwrap();
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi"),
            503,
            JSON,
            &bad("snapshot timed out"),
        )
        .await;
        lock.rollback().await.unwrap();
        until_idle(&pool).await;
        let after = common::get(&app, &format!("{ROUTE}?ticker=ordi")).await;
        assert_eq!(after.status, 200, "{}", after.body);
    })
    .await;
}

#[tokio::test]
async fn a_client_that_goes_away_frees_its_connection_and_slot() {
    with_history_pool("gone", 10, |app, pool| async move {
        let uri = format!("{ROUTE}?ticker=huge&format=csv");
        let response = send(&app, &uri).await;
        let mut body = response.into_body();
        body.frame().await.unwrap().unwrap();
        drop(body);
        until_idle(&pool).await;
        // Every slot is free again.
        let mut running = Vec::new();
        for _ in 0..4 {
            let response = send(&app, &uri).await;
            assert_eq!(response.status(), 200);
            running.push(response);
        }
        drop(running);
        until_idle(&pool).await;
    })
    .await;
}

async fn send(app: &Router, uri: &str) -> Response {
    let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    app.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn at_most_four_snapshots_run_at_once() {
    with_history_pool("cap", 10, |app, pool| async move {
        // A snapshot larger than its buffers waits for its client, holding
        // its slot.
        let uri = format!("{ROUTE}?ticker=huge");
        // Four answers whose bodies are not read yet.
        let mut running = Vec::new();
        for _ in 0..4 {
            let response = send(&app, &uri).await;
            assert_eq!(response.status(), 200);
            running.push(response);
        }
        let busy = bad("too many snapshots running, retry later");
        expect(&app, &uri, 503, JSON, &busy).await;
        expect(
            &app,
            &format!("{ROUTE}?ticker=ordi&format=csv"),
            503,
            JSON,
            &busy,
        )
        .await;
        // A body read to its end frees its slot by the time it ends.
        let done = running.pop().unwrap();
        let body = to_bytes(done.into_body(), usize::MAX).await.unwrap();
        assert!(body.ends_with(b"]}}"));
        let response = send(&app, &uri).await;
        assert_eq!(response.status(), 200);
        running.push(response);
        expect(&app, &uri, 503, JSON, &busy).await;
        // So do clients that go away.
        running.clear();
        until_idle(&pool).await;
        let again = common::get(&app, &uri).await;
        assert_eq!(again.status, 200);
    })
    .await;
}
