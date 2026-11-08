//! The brc20 routes over HTTP. Expected statuses and bodies are the
//! answers of the Node API's brc20 routes (the API's wire contract).
//!
//! The first tests use a database double that answers only the latest block
//! height, so every other query fails with the 500 answer. The live tests
//! run against OPI_API_TEST_DATABASE_URL (a brc20 database, read only) and
//! are skipped when it is unset.

use {
    axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, StatusCode},
    },
    opi_api::{
        config::{Module, load_config},
        db::Db,
        js::JsValue,
        router,
    },
    sqlx::postgres::PgPoolOptions,
    std::{collections::HashMap, net::SocketAddr},
    tower::ServiceExt,
};

const HTML: &str = "text/html; charset=utf-8";
const JSON: &str = "application/json; charset=utf-8";
const INTERNAL_ERROR: &str = r#"{"error":"internal error","result":null}"#;
const NOT_INDEXED: &str = r#"{"error":"block not indexed yet","result":null}"#;

fn app(db: Db) -> Router {
    let env: HashMap<String, String> = [("OPI_MODULES", "brc20"), ("DB_DATABASE", "b")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let config = load_config(|key| env.get(key).cloned()).unwrap();
    router(&config, vec![(Module::Brc20, db)])
}

fn height(value: JsValue) -> Router {
    app(Db::fake(move |_| Ok(value.clone())))
}

struct Answer {
    status: StatusCode,
    content_type: Option<String>,
    body: String,
}

async fn get(app: &Router, uri: &str) -> Answer {
    let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        content_type,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

async fn expect(app: &Router, uri: &str, status: u16, content_type: &str, body: &str) {
    let answer = get(app, uri).await;
    assert_eq!(answer.status.as_u16(), status, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(content_type), "{uri}");
    assert_eq!(answer.body, body, "{uri}");
}

#[tokio::test]
async fn ip_and_heights_are_text() {
    let app = height(328239.into());
    expect(&app, "/v1/brc20/ip", 200, HTML, "127.0.0.1").await;
    expect(&app, "/v1/brc20/block_height", 200, HTML, "328239").await;
    // `null + ''` for an empty table
    expect(
        &height(JsValue::Null),
        "/v1/brc20/block_height",
        200,
        HTML,
        "null",
    )
    .await;
    // get_block_height_of_db answers -1 when its query fails
    let down = app_down();
    expect(&down, "/v1/brc20/block_height", 200, HTML, "-1").await;
    // the extras query fails on the double, and its catch answers -1
    expect(&app, "/v1/brc20/extras_block_height", 200, HTML, "-1").await;
}

fn app_down() -> Router {
    app(Db::fake(|_| anyhow::bail!("connection refused")))
}

#[tokio::test]
async fn a_failed_query_answers_500() {
    let app = height(7.into());
    for uri in [
        "/v1/brc20/db_version",
        "/v1/brc20/event_hash_version",
        "/v1/brc20/balance_on_block?block_height=8&pkscript=00&ticker=ORDI",
        "/v1/brc20/activity_on_block?block_height=7",
        "/v1/brc20/bitcoin_rpc_results_on_block?block_height=7",
        "/v1/brc20/get_current_balance_of_wallet?ticker=x",
        "/v1/brc20/get_valid_tx_notes_of_wallet",
        "/v1/brc20/get_valid_tx_notes_of_ticker?ticker=x",
        "/v1/brc20/holders?ticker=x",
        "/v1/brc20/get_hash_of_all_activity?block_height=7",
        "/v1/brc20/get_hash_of_all_current_balances",
        "/v1/brc20/event?inscription_id=x",
        // the event types are read before inscription_id is checked
        "/v1/brc20/event",
        "/v1/brc20/explorer/btc_address/0xAB",
        "/v1/brc20/explorer/balances/0xab",
        "/v1/brc20/explorer/address_tabs_counters/0xab",
        "/v1/brc20/explorer/tokens",
        "/v1/brc20/explorer/tokens?order[toString]=x",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn a_missing_or_non_string_ticker_throws() {
    // request.query.ticker.toLowerCase() on undefined, an array or an object
    let app = height(7.into());
    for uri in [
        "/v1/brc20/balance_on_block?block_height=1",
        "/v1/brc20/balance_on_block?block_height=99&ticker[]=a",
        "/v1/brc20/get_current_balance_of_wallet?pkscript=00",
        "/v1/brc20/get_current_balance_of_wallet?ticker=a&ticker=b",
        "/v1/brc20/get_valid_tx_notes_of_ticker",
        "/v1/brc20/holders",
        "/v1/brc20/holders?ticker[a]=x",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn heights_compare_as_javascript_does() {
    let app = height(7.into());
    // balance_on_block: block_height > current + 1
    expect(
        &app,
        "/v1/brc20/balance_on_block?block_height=9&ticker=x",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
    for uri in [
        "/v1/brc20/balance_on_block?block_height=8&ticker=x",
        "/v1/brc20/balance_on_block?block_height=abc&ticker=x",
        "/v1/brc20/balance_on_block?ticker=x",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
    // the others: block_height > current
    for route in [
        "activity_on_block",
        "bitcoin_rpc_results_on_block",
        "get_hash_of_all_activity",
    ] {
        for (query, status) in [
            ("block_height=8", 400),
            ("block_height=%208%20", 400),
            ("block_height=1e1", 400),
            ("block_height[]=9", 400),
            ("block_height=7", 500),
            ("block_height=abc", 500),
            ("block_height=8&block_height=9", 500),
            ("", 500),
        ] {
            let body = if status == 400 {
                NOT_INDEXED
            } else {
                INTERNAL_ERROR
            };
            expect(
                &app,
                &format!("/v1/brc20/{route}?{query}"),
                status,
                JSON,
                body,
            )
            .await;
        }
    }
    // an empty table's null height: null + 1 is 1, and null compares as 0
    let empty = height(JsValue::Null);
    expect(
        &empty,
        "/v1/brc20/balance_on_block?block_height=2&ticker=x",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
    expect(
        &empty,
        "/v1/brc20/activity_on_block?block_height=1",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
    // a failed height query is -1
    expect(
        &app_down(),
        "/v1/brc20/get_hash_of_all_activity?block_height=0",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
}

#[tokio::test]
async fn explorer_paths_need_an_address() {
    let app = height(7.into());
    let page = |path: &str| {
        format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET {path}</pre>\n</body>\n</html>\n"
        )
    };
    for path in [
        "/v1/brc20/explorer/btc_address/",
        "/v1/brc20/explorer/balances",
        "/v1/brc20/explorer/address_tabs_counters/a/b",
    ] {
        expect(&app, path, 404, HTML, &page(path)).await;
    }
    // a bad escape in the address is Express's decode_param error
    expect(
        &app,
        "/v1/brc20/explorer/btc_address/%zz",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}

// Live tests against a brc20 database.

async fn live() -> Option<(Router, sqlx::PgPool)> {
    let Ok(url) = std::env::var("OPI_API_TEST_DATABASE_URL") else {
        eprintln!("OPI_API_TEST_DATABASE_URL is unset; skipping");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_lazy(&url)
        .unwrap();
    Some((app(Db::from_pool(pool.clone())), pool))
}

/// No repository holds the DDL of brc20_unused_tx_inscrs, so a NULL in any
/// column the API reads keeps JavaScript's null. The test builds it and
/// evm_addresses as temporary tables with nullable columns on its one session
/// (they shadow any real ones and vanish with the connection), so the
/// database is unchanged.
#[tokio::test]
async fn live_null_columns_read_as_null() {
    let Ok(url) = std::env::var("OPI_API_TEST_DATABASE_URL") else {
        eprintln!("OPI_API_TEST_DATABASE_URL is unset; skipping");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&url)
        .unwrap();
    for statement in [
        "create temporary table evm_addresses (evm_address text, btc_address text)",
        "insert into evm_addresses values ('0xab', null)",
        "create temporary table brc20_unused_tx_inscrs (
            id bigserial, inscription_id text, tick varchar(4), amount numeric(40),
            current_holder_pkscript text, current_holder_wallet text, event_id int8,
            block_height int4)",
        "insert into brc20_unused_tx_inscrs (current_holder_pkscript, current_holder_wallet, tick)
            values ('00', 'w', 'nul'), (null, null, 'nul'), ('01', null, null)",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    let app = app(Db::from_pool(pool));
    for (uri, body) in [
        (
            "/v1/brc20/get_valid_tx_notes_of_wallet?pkscript=01",
            r#"{"error":null,"result":{"unused_txes":[{"tick":null,"inscription_id":null,"amount":null,"genesis_height":null}],"block_height":"#.to_string(),
        ),
        (
            "/v1/brc20/explorer/btc_address/0xAB",
            r#"{"btc_address":null}"#.to_string(),
        ),
        (
            "/v1/brc20/explorer/balances/0xAB",
            r#"{"items":[]}"#.to_string(),
        ),
        (
            "/v1/brc20/explorer/address_tabs_counters/0xAB",
            r#"{"internal_transactions_count":0,"logs_count":0,"token_balances_count":0,"token_transfers_count":0,"transactions_count":0,"validations_count":0,"withdrawals_count":0}"#.to_string(),
        ),
        (
            "/v1/brc20/get_valid_tx_notes_of_wallet?pkscript=00",
            r#"{"error":null,"result":{"unused_txes":[{"tick":"nul","inscription_id":null,"amount":null,"genesis_height":null}],"block_height":"#.to_string(),
        ),
        (
            "/v1/brc20/get_valid_tx_notes_of_wallet?address=w",
            r#"{"error":null,"result":{"unused_txes":[{"tick":"nul","inscription_id":null,"amount":null,"genesis_height":null}],"block_height":"#.to_string(),
        ),
        (
            "/v1/brc20/get_valid_tx_notes_of_ticker?ticker=NUL",
            r#"{"error":null,"result":{"unused_txes":[{"current_holder_pkscript":"00","current_holder_wallet":"w","inscription_id":null,"amount":null,"genesis_height":null},{"current_holder_pkscript":null,"current_holder_wallet":null,"inscription_id":null,"amount":null,"genesis_height":null}],"block_height":"#.to_string(),
        ),
    ] {
        let answer = get(&app, uri).await;
        assert_eq!(answer.status, StatusCode::OK, "{uri}: {}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
        assert!(answer.body.starts_with(&body), "{uri}: {}", answer.body);
    }
}

#[tokio::test]
async fn live_versions_and_heights() {
    let Some((app, pool)) = live().await else {
        return;
    };
    let (db_version, event_hash_version, max): (i32, i32, Option<i32>) = sqlx::query_as(
        "select db_version, event_hash_version,
                (select max(block_height) from brc20_block_hashes)
         from brc20_indexer_version",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let max = max.map_or("null".to_string(), |m| m.to_string());
    expect(
        &app,
        "/v1/brc20/db_version",
        200,
        HTML,
        &db_version.to_string(),
    )
    .await;
    expect(
        &app,
        "/v1/brc20/event_hash_version",
        200,
        HTML,
        &event_hash_version.to_string(),
    )
    .await;
    expect(&app, "/v1/brc20/block_height", 200, HTML, &max).await;
}

#[tokio::test]
async fn live_current_balances_hash_matches_one_computed_in_sql() {
    let Some((app, pool)) = live().await else {
        return;
    };
    // the same string and hash, built by Postgres: UTF-8 byte order is
    // UTF-16 order unless a string holds a character in U+E000..U+FFFF
    // (UTF-16 puts those after the surrogate pairs)
    let (hash, upper_bmp): (String, i64) = sqlx::query_as(
        r#"select encode(sha256(convert_to(coalesce(string_agg(
                    pkscript || ';' || tick || ';' || overall_balance || ';' || available_balance,
                    '|' order by pkscript collate "C", tick collate "C"), ''), 'UTF8')), 'hex'),
                  count(*) filter (where pkscript || tick ~ '[-￿]')
           from brc20_current_balances where overall_balance <> 0"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(upper_bmp, 0);
    let answer = get(&app, "/v1/brc20/get_hash_of_all_current_balances").await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(
        answer
            .body
            .contains(&format!(r#""current_balances_hash":"{hash}""#)),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn live_routes_answer_their_shapes() {
    let Some((app, pool)) = live().await else {
        return;
    };
    let (height, inscription_id): (i32, String) = sqlx::query_as(
        "select block_height, inscription_id from brc20_events order by id desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (pkscript, tick): (String, String) = sqlx::query_as(
        "select pkscript, tick from brc20_current_balances where overall_balance > 0 limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    for (uri, prefix) in [
        (
            format!("/v1/brc20/activity_on_block?block_height={height}"),
            r#"{"error":null,"result":[{"#,
        ),
        (
            format!("/v1/brc20/event?inscription_id={inscription_id}"),
            r#"{"error":null,"result":[{"#,
        ),
        (
            format!("/v1/brc20/get_hash_of_all_activity?block_height={height}"),
            r#"{"error":null,"result":{"cumulative_event_hash":""#,
        ),
        (
            format!("/v1/brc20/get_current_balance_of_wallet?pkscript={pkscript}&ticker={tick}"),
            r#"{"error":null,"result":{"overall_balance":""#,
        ),
        (
            format!("/v1/brc20/holders?ticker={tick}"),
            r#"{"error":null,"result":{"unused_txes":[{"pkscript":""#,
        ),
        (
            "/v1/brc20/explorer/tokens?order=asc".to_string(),
            r#"{"items":[{"address":"","type":"ERC-20","symbol":""#,
        ),
    ] {
        let answer = get(&app, &uri).await;
        assert_eq!(answer.status, StatusCode::OK, "{uri}: {}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some(JSON));
        assert!(answer.body.starts_with(prefix), "{uri}: {}", answer.body);
    }
    // a height that is not an integer is Postgres's input error
    expect(
        &app,
        "/v1/brc20/activity_on_block?block_height=1.5",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}
