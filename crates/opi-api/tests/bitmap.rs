//! The bitmap routes over HTTP. Expected statuses and bodies are the
//! answers of the Node API's bitmap routes (the API's wire contract).
//!
//! The first tests use a database double that answers only the latest block
//! height, so every other query fails with the 500 answer. The live tests
//! run against OPI_API_TEST_BITMAP_URL (a bitmap database, read only) and
//! are skipped when it is unset.

mod common;

use {
    common::{
        HTML, INTERNAL_ERROR, JSON, NOT_INDEXED, down, expect, get, height, js_string, live,
        not_found_page,
    },
    opi_api::{config::Module, js::JsValue},
};

const BITMAP: Module = Module::Bitmap;

#[tokio::test]
async fn ip_and_height_are_text() {
    let app = height(BITMAP, 328209.into());
    expect(&app, "/v1/bitmap/ip", 200, HTML, "127.0.0.1").await;
    expect(&app, "/v1/bitmap/block_height", 200, HTML, "328209").await;
    expect(
        &height(BITMAP, JsValue::Null),
        "/v1/bitmap/block_height",
        200,
        HTML,
        "null",
    )
    .await;
    // get_block_height_of_db has no catch here, so a failed height is 500
    expect(
        &down(BITMAP),
        "/v1/bitmap/block_height",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}

#[tokio::test]
async fn a_failed_query_answers_500() {
    let app = height(BITMAP, 7.into());
    for uri in [
        "/v1/bitmap/get_hash_of_all_activity?block_height=7",
        "/v1/bitmap/get_hash_of_all_activity",
        "/v1/bitmap/get_hash_of_all_bitmaps",
        "/v1/bitmap/get_inscription_id_of_bitmap?bitmap_number=1",
        "/v1/bitmap/get_inscription_id_of_bitmap",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
    let down = down(BITMAP);
    for uri in [
        "/v1/bitmap/get_hash_of_all_activity?block_height=99",
        "/v1/bitmap/get_hash_of_all_bitmaps",
    ] {
        expect(&down, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn heights_compare_as_javascript_does() {
    let app = height(BITMAP, 7.into());
    for (query, status) in [
        ("block_height=8", 400),
        ("block_height=%208%20", 400),
        ("block_height=1e1", 400),
        ("block_height[]=9", 400),
        ("block_height=7", 500),
        ("block_height=abc", 500),
        ("block_height=8&block_height=9", 500),
    ] {
        let body = if status == 400 {
            NOT_INDEXED
        } else {
            INTERNAL_ERROR
        };
        let uri = format!("/v1/bitmap/get_hash_of_all_activity?{query}");
        expect(&app, &uri, status, JSON, body).await;
    }
    // an empty table's null height compares as 0
    expect(
        &height(BITMAP, JsValue::Null),
        "/v1/bitmap/get_hash_of_all_activity?block_height=1",
        400,
        JSON,
        NOT_INDEXED,
    )
    .await;
}

#[tokio::test]
async fn unknown_paths_answer_the_404_page() {
    let app = height(BITMAP, 7.into());
    for path in ["/v1/bitmap/no_such_route", "/v1/bitmap"] {
        expect(&app, path, 404, HTML, &not_found_page(path)).await;
    }
}

// Live tests against a bitmap database.

#[tokio::test]
async fn live_routes_answer_the_rows() {
    let Some((app, pool)) = live(BITMAP, "OPI_API_TEST_BITMAP_URL") else {
        return;
    };
    let (max, cumulative, block, version): (Option<i32>, String, String, String) = sqlx::query_as(
        "select h.block_height, h.cumulative_event_hash, h.block_event_hash,
                    (select indexer_version from bitmap_indexer_version)
             from bitmap_cumulative_event_hashes h
             where h.block_height = (select max(block_height) from bitmap_block_hashes)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let max = max.unwrap();
    expect(&app, "/v1/bitmap/block_height", 200, HTML, &max.to_string()).await;
    // the requested height comes back as the query string sent it
    expect(
        &app,
        &format!("/v1/bitmap/get_hash_of_all_activity?block_height=%20{max}"),
        200,
        JSON,
        &format!(
            r#"{{"error":null,"result":{{"cumulative_event_hash":{},"block_event_hash":{},"indexer_version":{},"block_height":" {max}"}}}}"#,
            js_string(&cumulative),
            js_string(&block),
            js_string(&version)
        ),
    )
    .await;
    // a height with no hash row: `res.rows[0].cumulative_event_hash` throws
    expect(
        &app,
        "/v1/bitmap/get_hash_of_all_activity?block_height=-5",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;

    let (number, inscription_id): (i32, String) =
        sqlx::query_as("select bitmap_number, inscription_id from bitmaps order by id limit 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    expect(
        &app,
        &format!("/v1/bitmap/get_inscription_id_of_bitmap?bitmap_number=+{number}"),
        200,
        JSON,
        &format!(
            r#"{{"error":null,"result":{{"inscription_id":{},"bitmap_number":" {number}"}}}}"#,
            js_string(&inscription_id)
        ),
    )
    .await;
    expect(
        &app,
        "/v1/bitmap/get_inscription_id_of_bitmap?bitmap_number=-1",
        400,
        JSON,
        r#"{"error":"bitmap not found","result":null}"#,
    )
    .await;
    for uri in [
        // Postgres refuses the text as an integer
        "/v1/bitmap/get_inscription_id_of_bitmap?bitmap_number=abc",
        "/v1/bitmap/get_inscription_id_of_bitmap?bitmap_number[]=1",
        // the route binds a parameter its query has no place for
        "/v1/bitmap/get_hash_of_all_bitmaps",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
    // no bitmap_number binds NULL, which matches no row
    let answer = get(&app, "/v1/bitmap/get_inscription_id_of_bitmap").await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}
