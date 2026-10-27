//! The sns routes over HTTP. Expected statuses and bodies are what
//! api/routes/sns.js answers for the same requests.
//!
//! The first tests use a database double that answers only the latest block
//! height, so every other query fails with the 500 answer. The live tests
//! run against OPI_API_TEST_SNS_URL (an sns database, read only) and are
//! skipped when it is unset.

mod common;

use {
    common::{
        HTML, INTERNAL_ERROR, JSON, NOT_INDEXED, down, encode, expect, get, height, js_string,
        live, not_found_page,
    },
    opi_api::{config::Module, js::JsValue},
};

const SNS: Module = Module::Sns;

#[tokio::test]
async fn ip_and_height_are_text() {
    let app = height(SNS, 328209.into());
    expect(&app, "/v1/sns/ip", 200, HTML, "127.0.0.1").await;
    expect(&app, "/v1/sns/block_height", 200, HTML, "328209").await;
    expect(
        &height(SNS, JsValue::Null),
        "/v1/sns/block_height",
        200,
        HTML,
        "null",
    )
    .await;
    // get_block_height_of_db has no catch here, so a failed height is 500
    expect(
        &down(SNS),
        "/v1/sns/block_height",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}

#[tokio::test]
async fn a_failed_query_answers_500() {
    let app = height(SNS, 7.into());
    for uri in [
        "/v1/sns/get_hash_of_all_activity?block_height=7",
        "/v1/sns/get_hash_of_all_registered_names",
        "/v1/sns/get_info_of_sns?name=a.sats",
        "/v1/sns/get_info_of_sns",
        "/v1/sns/get_inscriptions_of_domain?domain=sats",
        "/v1/sns/get_registered_namespaces",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
    let down = down(SNS);
    for uri in [
        "/v1/sns/get_hash_of_all_activity?block_height=99",
        "/v1/sns/get_hash_of_all_registered_names",
    ] {
        expect(&down, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

#[tokio::test]
async fn heights_compare_as_javascript_does() {
    let app = height(SNS, 7.into());
    for (query, status) in [
        ("block_height=8", 400),
        ("block_height=0x10", 400),
        ("block_height[a]=9", 500),
        ("block_height=7", 500),
        ("block_height=", 500),
    ] {
        let body = if status == 400 {
            NOT_INDEXED
        } else {
            INTERNAL_ERROR
        };
        let uri = format!("/v1/sns/get_hash_of_all_activity?{query}");
        expect(&app, &uri, status, JSON, body).await;
    }
}

#[tokio::test]
async fn unknown_paths_answer_the_404_page() {
    let app = height(SNS, 7.into());
    let path = "/v1/sns/no_such_route";
    expect(&app, path, 404, HTML, &not_found_page(path)).await;
}

// Live tests against an sns database.

#[tokio::test]
async fn live_hashes_match_ones_computed_in_sql() {
    let Some((app, pool)) = live(SNS, "OPI_API_TEST_SNS_URL") else {
        return;
    };
    // the same string and hash, built by Postgres
    let (names_hash, version, max, cumulative, block): (String, String, i32, String, String) =
        sqlx::query_as(
            r#"select encode(sha256(convert_to(coalesce(string_agg(
                    "name" || ';' || domain || ';' || inscription_id || ';' || inscription_number,
                    '|' order by inscription_number), ''), 'UTF8')), 'hex'),
                  (select indexer_version from sns_names_indexer_version),
                  (select max(block_height) from sns_block_hashes),
                  (select cumulative_event_hash from sns_names_cumulative_event_hashes
                   where block_height = (select max(block_height) from sns_block_hashes)),
                  (select block_event_hash from sns_names_cumulative_event_hashes
                   where block_height = (select max(block_height) from sns_block_hashes))
           from sns_names"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    expect(
        &app,
        "/v1/sns/get_hash_of_all_registered_names",
        200,
        JSON,
        &format!(
            r#"{{"error":null,"result":{{"current_sns_names_hash":"{names_hash}","indexer_version":{},"block_height":{max}}}}}"#,
            js_string(&version)
        ),
    )
    .await;
    expect(
        &app,
        &format!("/v1/sns/get_hash_of_all_activity?block_height={max}"),
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
}

#[tokio::test]
async fn live_lookups_answer_the_rows() {
    let Some((app, pool)) = live(SNS, "OPI_API_TEST_SNS_URL") else {
        return;
    };
    let (name, domain, inscription_id, number): (String, String, String, i32) = sqlx::query_as(
        r#"select "name", domain, inscription_id, inscription_number from sns_names order by id limit 1"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    expect(
        &app,
        &format!("/v1/sns/get_info_of_sns?name={}", encode(&name)),
        200,
        JSON,
        &format!(
            r#"{{"error":null,"result":{{"inscription_id":{},"inscription_number":{number},"domain":{},"sns_name":{}}}}}"#,
            js_string(&inscription_id),
            js_string(&domain),
            js_string(&name)
        ),
    )
    .await;
    expect(
        &app,
        "/v1/sns/get_info_of_sns?name=no%20such%20name",
        400,
        JSON,
        r#"{"error":"sns name not found","result":null}"#,
    )
    .await;
    let answer = get(
        &app,
        &format!(
            "/v1/sns/get_inscriptions_of_domain?domain={}",
            encode(&domain)
        ),
    )
    .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON));
    assert!(
        answer
            .body
            .starts_with(r#"{"error":null,"result":[{"inscription_id":""#),
        "{}",
        answer.body
    );
    expect(
        &app,
        "/v1/sns/get_inscriptions_of_domain",
        200,
        JSON,
        r#"{"error":null,"result":[]}"#,
    )
    .await;
    let answer = get(&app, "/v1/sns/get_registered_namespaces").await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer
            .body
            .starts_with(r#"{"error":null,"result":[{"inscription_id":""#),
        "{}",
        answer.body
    );
    assert!(answer.body.contains(r#","namespace":""#), "{}", answer.body);
}
