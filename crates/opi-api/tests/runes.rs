//! The runes routes over HTTP. Expected statuses and bodies are what
//! api/routes/runes.js answers for the same requests.
//!
//! The first tests use a database double that answers only the latest block
//! height, so every other query fails with the 500 answer. The live tests
//! run against OPI_API_TEST_RUNES_URL (a runes database, read only) and are
//! skipped when it is unset.

mod common;

use {
    common::{
        HTML, INTERNAL_ERROR, JSON, NOT_INDEXED, down, encode, expect, get, height, js_string,
        live, not_found_page,
    },
    opi_api::{config::Module, js::JsValue},
};

const RUNES: Module = Module::Runes;
const RUNE_NOT_FOUND: &str = r#"{"error":"rune not found","result":null}"#;

#[tokio::test]
async fn ip_and_height_are_text() {
    let app = height(RUNES, 328209.into());
    expect(&app, "/v1/runes/ip", 200, HTML, "127.0.0.1").await;
    expect(&app, "/v1/runes/block_height", 200, HTML, "328209").await;
    expect(
        &height(RUNES, JsValue::Null),
        "/v1/runes/block_height",
        200,
        HTML,
        "null",
    )
    .await;
    // get_block_height_of_db answers -1 when its query fails
    expect(&down(RUNES), "/v1/runes/block_height", 200, HTML, "-1").await;
}

#[tokio::test]
async fn a_failed_query_answers_500() {
    let app = height(RUNES, 7.into());
    for uri in [
        "/v1/runes/balance_on_block?block_height=8&pkscript=00",
        "/v1/runes/balance_on_block",
        "/v1/runes/activity_on_block?block_height=7",
        "/v1/runes/get_current_balance_of_wallet?address=tb1",
        "/v1/runes/get_current_balance_of_wallet",
        "/v1/runes/get_unspent_rune_outpoints_of_wallet?pkscript=00",
        "/v1/runes/holders?rune_name=abc",
        "/v1/runes/holders?rune_name=&rune_id=1:1",
        "/v1/runes/get_hash_of_all_activity?block_height=7",
        "/v1/runes/event?transaction_id=00",
        // the event types are read before transaction_id is checked
        "/v1/runes/event",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn a_missing_or_non_string_rune_name_throws() {
    // request.query.rune_name.toUpperCase() on undefined, an array or an object
    let app = height(RUNES, 7.into());
    for uri in [
        "/v1/runes/holders",
        "/v1/runes/holders?rune_id=1:1",
        "/v1/runes/holders?rune_name[]=a",
        "/v1/runes/holders?rune_name=a&rune_name=b",
        "/v1/runes/holders?rune_name[a]=b",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn heights_compare_as_javascript_does() {
    let app = height(RUNES, 7.into());
    // balance_on_block: block_height > current + 1
    expect(
        &app,
        "/v1/runes/balance_on_block?block_height=9",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
    expect(
        &app,
        "/v1/runes/balance_on_block?block_height=8",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
    // the others: block_height > current
    for route in ["activity_on_block", "get_hash_of_all_activity"] {
        for (query, status) in [
            ("block_height=8", 400),
            ("block_height=%208%20", 400),
            ("block_height[]=9", 400),
            ("block_height=7", 500),
            ("block_height=abc", 500),
            ("", 500),
        ] {
            let body = if status == 400 {
                NOT_INDEXED
            } else {
                INTERNAL_ERROR
            };
            let uri = format!("/v1/runes/{route}?{query}");
            expect(&app, &uri, status, JSON, body).await;
        }
    }
    // a failed height query is -1, and an empty table's null + 1 is 1
    expect(
        &down(RUNES),
        "/v1/runes/get_hash_of_all_activity?block_height=0",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
    expect(
        &height(RUNES, JsValue::Null),
        "/v1/runes/balance_on_block?block_height=2",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
}

#[tokio::test]
async fn unknown_paths_answer_the_404_page() {
    let app = height(RUNES, 7.into());
    let path = "/v1/runes/no_such_route";
    expect(&app, path, 404, HTML, &not_found_page(path)).await;
}

// Live tests against a runes database.

#[tokio::test]
async fn live_heights_and_hashes() {
    let Some((app, pool)) = live(RUNES, "OPI_API_TEST_RUNES_URL") else {
        return;
    };
    let (max, cumulative, block, version): (i32, String, String, String) = sqlx::query_as(
        "select h.block_height, h.cumulative_event_hash, h.block_event_hash,
                (select indexer_version from runes_indexer_version)
         from runes_cumulative_event_hashes h
         where h.block_height = (select max(block_height) from runes_block_hashes)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    expect(&app, "/v1/runes/block_height", 200, HTML, &max.to_string()).await;
    expect(
        &app,
        &format!("/v1/runes/get_hash_of_all_activity?block_height={max}"),
        200,
        JSON,
        &format!(
            r#"{{"error":null,"result":{{"cumulative_event_hash":{},"block_event_hash":{},"indexer_version":{},"block_height":"{max}"}}}}"#,
            js_string(&cumulative),
            js_string(&block),
            js_string(&version)
        ),
    )
    .await;
    expect(
        &app,
        "/v1/runes/get_hash_of_all_activity?block_height=-5",
        400,
        JSON,
        r#"{"error":"block not indexed","result":null}"#,
    )
    .await;
}

#[tokio::test]
async fn live_holders_by_name_and_by_id_agree() {
    let Some((app, pool)) = live(RUNES, "OPI_API_TEST_RUNES_URL") else {
        return;
    };
    let (rune_id, rune_name): (String, String) = sqlx::query_as(
        "select e.rune_id, e.rune_name from runes_id_to_entry e
         where exists (select 1 from runes_outpoint_to_balances b
                       where e.rune_id = any(b.rune_ids) and not b.spent)
         order by e.id limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let by_name = get(
        &app,
        &format!(
            "/v1/runes/holders?rune_name={}",
            encode(&rune_name.to_lowercase())
        ),
    )
    .await;
    let by_id = get(
        &app,
        &format!("/v1/runes/holders?rune_name=&rune_id={}", encode(&rune_id)),
    )
    .await;
    assert_eq!(by_name.status, 200, "{}", by_name.body);
    assert_eq!(by_name.content_type.as_deref(), Some(JSON));
    assert_eq!(by_name.body, by_id.body);
    assert!(
        by_name
            .body
            .starts_with(r#"{"error":null,"result":[{"pkscript":""#),
        "{}",
        by_name.body
    );
    assert!(
        by_name
            .body
            .contains(&format!(r#","rune_name":{}}}"#, js_string(&rune_name))),
        "{}",
        by_name.body
    );
    for uri in [
        "/v1/runes/holders?rune_name=NO%20SUCH%20RUNE",
        "/v1/runes/holders?rune_name=&rune_id=0:0",
        "/v1/runes/holders?rune_name=",
    ] {
        expect(&app, uri, 400, JSON, RUNE_NOT_FOUND).await;
    }
}

/// node-pg's statements are unnamed, so Postgres plans each execution with
/// its parameter values. A grouped query without an order answers in its
/// plan's order, so a repeated request must keep the first answer's order
/// after the fifth execution, where a cached statement may switch to a
/// generic plan.
#[tokio::test]
async fn live_repeated_requests_keep_the_row_order() {
    let Some((app, pool)) = live(RUNES, "OPI_API_TEST_RUNES_URL") else {
        return;
    };
    let wallet: String = sqlx::query_scalar(
        "select wallet_addr from runes_outpoint_to_balances
         where not spent and wallet_addr is not null
         group by 1 order by count(*) desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let uri = format!("/v1/runes/get_current_balance_of_wallet?address={wallet}");
    let first = get(&app, &uri).await.body;
    // two pooled connections, each past its fifth execution
    for run in 2..=15 {
        assert_eq!(get(&app, &uri).await.body, first, "run {run}");
    }
}

#[tokio::test]
async fn live_wallet_routes_answer_their_shapes() {
    let Some((app, pool)) = live(RUNES, "OPI_API_TEST_RUNES_URL") else {
        return;
    };
    let (pkscript, wallet, txid, event_height): (String, String, String, i32) = sqlx::query_as(
        "select b.pkscript, b.wallet_addr, e.txid, e.block_height
         from runes_outpoint_to_balances b, runes_events e
         where not b.spent and b.wallet_addr is not null and e.pkscript = b.pkscript
         order by b.id limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let max: i32 = sqlx::query_scalar("select max(block_height) from runes_block_hashes")
        .fetch_one(&pool)
        .await
        .unwrap();
    let by_pkscript = get(
        &app,
        &format!("/v1/runes/get_current_balance_of_wallet?pkscript={pkscript}"),
    )
    .await;
    let by_wallet = get(
        &app,
        &format!("/v1/runes/get_current_balance_of_wallet?address={wallet}"),
    )
    .await;
    assert_eq!(by_pkscript.status, 200, "{}", by_pkscript.body);
    assert!(
        by_pkscript
            .body
            .starts_with(r#"{"error":null,"result":[{"pkscript":""#),
        "{}",
        by_pkscript.body
    );
    assert!(
        by_pkscript
            .body
            .ends_with(&format!(r#"],"db_block_height":{max}}}"#)),
        "{}",
        by_pkscript.body
    );
    assert!(by_pkscript.body.contains(r#","total_balance":""#));
    assert_eq!(by_wallet.status, 200, "{}", by_wallet.body);
    assert!(
        by_wallet
            .body
            .contains(&format!(r#""wallet_addr":{}"#, js_string(&wallet))),
        "{}",
        by_wallet.body
    );

    let outpoints = get(
        &app,
        &format!("/v1/runes/get_unspent_rune_outpoints_of_wallet?pkscript={pkscript}"),
    )
    .await;
    assert_eq!(outpoints.status, 200, "{}", outpoints.body);
    // rune_ids is a text[] of strings, balances a numeric[] of numbers
    assert!(
        outpoints.body.contains(r#","rune_ids":[""#),
        "{}",
        outpoints.body
    );
    let balances = outpoints.body.split(r#","balances":["#).nth(1).unwrap();
    assert!(
        balances.starts_with(|c: char| c.is_ascii_digit()),
        "{}",
        outpoints.body
    );

    let balance = get(
        &app,
        &format!("/v1/runes/balance_on_block?block_height={max}&pkscript={pkscript}"),
    )
    .await;
    assert_eq!(balance.status, 200, "{}", balance.body);
    assert!(balance.body.starts_with(r#"{"error":null,"result":["#));

    let activity = get(
        &app,
        &format!("/v1/runes/activity_on_block?block_height={event_height}"),
    )
    .await;
    assert_eq!(activity.status, 200, "{}", activity.body);
    assert!(
        activity
            .body
            .starts_with(r#"{"error":null,"result":[{"event_type":""#),
        "{}",
        activity.body
    );
    let event = get(&app, &format!("/v1/runes/event?transaction_id={txid}")).await;
    assert_eq!(event.status, 200, "{}", event.body);
    assert!(event.body.contains(r#","wallet_addr":"#), "{}", event.body);
    assert!(event.body.contains(r#","amount":""#), "{}", event.body);
    expect(
        &app,
        "/v1/runes/event",
        400,
        JSON,
        r#"{"error":"transaction_id is required","result":null}"#,
    )
    .await;
    expect(
        &app,
        "/v1/runes/balance_on_block?block_height=1.5",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}
