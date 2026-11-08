//! `/v1/bitmap/by_inscription` and `/v1/bitmap/search` over HTTP.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters are checked before any query, and a valid request
//! answers the 500 body when its query fails. The seeded tests build a
//! throwaway database from opi-bitmap's db_init.sql on the local Postgres
//! and drop it afterwards; they skip unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{INTERNAL_ERROR, JSON, expect, get, height, with_seeded},
    opi_api::config::Module,
};

const BITMAP: Module = Module::Bitmap;

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(BITMAP, 7.into());
    let prefix = "prefix must be 1 to 10 decimal digits";
    let limit = "limit must be an integer from 1 to 500";
    let after = "after must be an integer from 0 to 2147483647";
    let long = format!(
        "/v1/bitmap/by_inscription?inscription_id={}",
        "a".repeat(129)
    );
    for (uri, reason) in [
        (
            "/v1/bitmap/by_inscription",
            "inscription_id is required".to_string(),
        ),
        (
            "/v1/bitmap/by_inscription?inscription_id=",
            "inscription_id is required".into(),
        ),
        (
            "/v1/bitmap/by_inscription?inscription_id=a&inscription_id=b",
            "inscription_id must be a single value".into(),
        ),
        (
            "/v1/bitmap/by_inscription?inscription_id=%00",
            "inscription_id must be at most 128 bytes without NUL".into(),
        ),
        (
            long.as_str(),
            "inscription_id must be at most 128 bytes without NUL".into(),
        ),
        ("/v1/bitmap/search", "prefix is required".into()),
        ("/v1/bitmap/search?prefix=", "prefix is required".into()),
        ("/v1/bitmap/search?prefix=a", prefix.into()),
        ("/v1/bitmap/search?prefix=-1", prefix.into()),
        ("/v1/bitmap/search?prefix=1.5", prefix.into()),
        ("/v1/bitmap/search?prefix=12345678901", prefix.into()),
        ("/v1/bitmap/search?prefix=1&limit=0", limit.into()),
        ("/v1/bitmap/search?prefix=1&limit=501", limit.into()),
        ("/v1/bitmap/search?prefix=1&after=-1", after.into()),
        ("/v1/bitmap/search?prefix=1&after=2147483648", after.into()),
        ("/v1/bitmap/search?prefix=1&after=x", after.into()),
        (
            "/v1/bitmap/search?prefix=1&prefix=2",
            "prefix must be a single value".into(),
        ),
    ] {
        expect(&app, uri, 400, JSON, &bad(&reason)).await;
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(BITMAP, 7.into());
    for uri in [
        "/v1/bitmap/by_inscription?inscription_id=abci0",
        "/v1/bitmap/search?prefix=1",
        "/v1/bitmap/search?prefix=0&after=5&limit=500",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
    // no range matches, yet the query still runs
    expect(
        &app,
        "/v1/bitmap/search?prefix=00",
        500,
        JSON,
        INTERNAL_ERROR,
    )
    .await;
}

const FIXTURE: &str = "
INSERT INTO bitmaps (inscription_id, inscription_number, bitmap_number, block_height)
VALUES ('i0', 10, 0, 800000), ('i1', 11, 1, 800001), ('i10', 12, 10, 800002),
       ('i11', 13, 11, 800003), ('i12', 14, 12, 800004), ('i100', 15, 100, 800005),
       ('i101', 16, 101, 800006), ('i120', 17, 120, 800007), ('i1000', 18, 1000, 800008),
       ('i1200', 19, 1200, 800009), ('i21', 20, 21, 800010), ('i22', 21, 22, 800011),
       ('i2b', 22, 2000000000, 800012), ('imax', 23, 2147483647, 800013),
       ('i999', 24, 999999999, 800014);
";

async fn seeded<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Router) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_seeded(
        BITMAP,
        tag,
        None,
        &[include_str!("../../opi-bitmap/src/db_init.sql"), FIXTURE],
        |app, _| body(app),
    )
    .await;
}

fn ok(result: &str) -> String {
    format!(r#"{{"error":null,"result":{result}}}"#)
}

/// The `bitmap_number`s of a search answer, in order, and its `has_more` and
/// `next_after`.
async fn page(app: &Router, uri: &str) -> (Vec<i64>, bool, Option<i64>) {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
    let numbers = answer
        .body
        .split(r#""bitmap_number":"#)
        .skip(1)
        .map(|rest| rest[..rest.find(',').unwrap()].parse().unwrap())
        .collect();
    let has_more = answer.body.contains(r#""has_more":true"#);
    let next_after = answer
        .body
        .split(r#""next_after":"#)
        .nth(1)
        .and_then(|rest| rest[..rest.find(',').unwrap()].parse().ok());
    (numbers, has_more, next_after)
}

#[tokio::test]
async fn seeded_by_inscription_answers_the_bitmap_or_404() {
    seeded("bm_insc", |app| async move {
        expect(
            &app,
            "/v1/bitmap/by_inscription?inscription_id=i120",
            200,
            JSON,
            &ok(
                r#"{"inscription_id":"i120","inscription_number":17,"bitmap_number":120,"block_height":800007}"#,
            ),
        )
        .await;
        expect(
            &app,
            "/v1/bitmap/by_inscription?inscription_id=i0",
            200,
            JSON,
            &ok(
                r#"{"inscription_id":"i0","inscription_number":10,"bitmap_number":0,"block_height":800000}"#,
            ),
        )
        .await;
        for id in ["nope", "I120", "i13", "%25", "i1_0"] {
            expect(
                &app,
                &format!("/v1/bitmap/by_inscription?inscription_id={id}"),
                404,
                JSON,
                &bad("bitmap not found"),
            )
            .await;
        }
    })
    .await;
}

#[tokio::test]
async fn seeded_search_lists_the_numbers_that_start_with_the_prefix() {
    seeded("bm_search", |app| async move {
        let search = |query: &str| format!("/v1/bitmap/search?{query}");
        let all_ones = vec![1, 10, 11, 12, 100, 101, 120, 1000, 1200];
        assert_eq!(
            page(&app, &search("prefix=1")).await,
            (all_ones.clone(), false, None)
        );
        assert_eq!(
            page(&app, &search("prefix=12")).await,
            (vec![12, 120, 1200], false, None)
        );
        assert_eq!(page(&app, &search("prefix=1200")).await.0, vec![1200]);
        assert_eq!(page(&app, &search("prefix=13")).await.0, Vec::<i64>::new());
        assert_eq!(
            page(&app, &search("prefix=2")).await.0,
            vec![21, 22, 2_000_000_000, 2_147_483_647]
        );
        assert_eq!(page(&app, &search("prefix=20")).await.0, vec![2_000_000_000]);
        assert_eq!(page(&app, &search("prefix=9")).await.0, vec![999_999_999]);
        assert_eq!(
            page(&app, &search("prefix=2147483647")).await.0,
            vec![2_147_483_647]
        );
        assert_eq!(
            page(&app, &search("prefix=2147483648")).await.0,
            Vec::<i64>::new()
        );
        // a number has no leading zeros: only 0 itself starts with `0`
        assert_eq!(page(&app, &search("prefix=0")).await.0, vec![0]);
        assert_eq!(page(&app, &search("prefix=00")).await.0, Vec::<i64>::new());
        assert_eq!(page(&app, &search("prefix=01")).await.0, Vec::<i64>::new());
        // the whole answer
        expect(
            &app,
            &search("prefix=1200"),
            200,
            JSON,
            &ok(
                r#"{"prefix":"1200","limit":100,"has_more":false,"next_after":null,"bitmaps":[{"inscription_id":"i1200","inscription_number":19,"bitmap_number":1200,"block_height":800009}]}"#,
            ),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn seeded_search_pages_by_key() {
    seeded("bm_paging", |app| async move {
        let search = |query: &str| format!("/v1/bitmap/search?{query}");
        assert_eq!(
            page(&app, &search("prefix=1&limit=3")).await,
            (vec![1, 10, 11], true, Some(11))
        );
        assert_eq!(
            page(&app, &search("prefix=1&limit=3&after=11")).await,
            (vec![12, 100, 101], true, Some(101))
        );
        assert_eq!(
            page(&app, &search("prefix=1&limit=3&after=101")).await,
            (vec![120, 1000, 1200], false, None)
        );
        // a page of exactly the rest has no more
        assert_eq!(
            page(&app, &search("prefix=1&limit=9")).await,
            (vec![1, 10, 11, 12, 100, 101, 120, 1000, 1200], false, None)
        );
        // the cursor need not be a listed number
        assert_eq!(
            page(&app, &search("prefix=1&limit=2&after=50")).await,
            (vec![100, 101], true, Some(101))
        );
        assert_eq!(
            page(&app, &search("prefix=1&after=1200")).await.0,
            Vec::<i64>::new()
        );
        // a cursor past one range skips it whole
        assert_eq!(
            page(&app, &search("prefix=2&limit=2&after=22")).await,
            (vec![2_000_000_000, 2_147_483_647], false, None)
        );
        // the hard cap
        let (numbers, _, _) = page(&app, &search("prefix=1&limit=500")).await;
        assert_eq!(numbers.len(), 9);
    })
    .await;
}
