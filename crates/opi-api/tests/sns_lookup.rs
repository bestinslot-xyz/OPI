//! `/v1/sns/by_inscription`, `/v1/sns/by_inscriptions` and `/v1/sns/search`
//! over HTTP.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters are checked before any query, and a valid request
//! answers the 500 body when its query fails. The seeded tests build a
//! throwaway database from opi-sns's db_init.sql on the local Postgres, one
//! in the `C` collation and one in en_US.UTF-8, and drop it afterwards; they
//! skip unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{INTERNAL_ERROR, JSON, encode, expect, get, height, js_string, post, with_seeded},
    opi_api::config::Module,
    sqlx::PgPool,
};

const SNS: Module = Module::Sns;

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(SNS, 7.into());
    let id = "inscription_id must be at most 128 bytes without NUL";
    let limit = "limit must be an integer from 1 to 500";
    let long_id = format!("/v1/sns/by_inscription?inscription_id={}", "a".repeat(129));
    let long_prefix = format!("/v1/sns/search?prefix={}", "a".repeat(2049));
    let long_namespace = format!("/v1/sns/search?prefix=a&namespace={}", "a".repeat(2049));
    for (uri, reason) in [
        ("/v1/sns/by_inscription", "inscription_id is required"),
        (
            "/v1/sns/by_inscription?inscription_id=",
            "inscription_id is required",
        ),
        ("/v1/sns/by_inscription?inscription_id=%00", id),
        (long_id.as_str(), id),
        (
            "/v1/sns/by_inscription?inscription_id[]=a",
            "inscription_id must be a single value",
        ),
        ("/v1/sns/search", "prefix is required"),
        ("/v1/sns/search?prefix=", "prefix is required"),
        (
            "/v1/sns/search?prefix=a&prefix=b",
            "prefix must be a single value",
        ),
        (
            "/v1/sns/search?prefix=a%00",
            "prefix must be at most 2048 bytes without NUL",
        ),
        (
            long_prefix.as_str(),
            "prefix must be at most 2048 bytes without NUL",
        ),
        (
            "/v1/sns/search?prefix=a&namespace=a%00",
            "namespace must be at most 2048 bytes without NUL",
        ),
        (
            long_namespace.as_str(),
            "namespace must be at most 2048 bytes without NUL",
        ),
        (
            "/v1/sns/search?prefix=a&namespace[x]=1",
            "namespace must be a single value",
        ),
        (
            "/v1/sns/search?prefix=a&after=a%00",
            "after must be at most 2048 bytes without NUL",
        ),
        ("/v1/sns/search?prefix=a&limit=0", limit),
        ("/v1/sns/search?prefix=a&limit=501", limit),
        ("/v1/sns/search?prefix=a&limit=x", limit),
    ] {
        expect(&app, uri, 400, JSON, &bad(reason)).await;
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(SNS, 7.into());
    for uri in [
        "/v1/sns/by_inscription?inscription_id=abci0",
        "/v1/sns/search?prefix=a",
        "/v1/sns/search?prefix=%25_%5C&namespace=SATS&after=ab.sats&limit=500",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

/// Names (inscription number order is the insertion order), and the
/// namespaces they use.
const FIXTURE: &str = r#"
INSERT INTO sns_names (inscription_id, inscription_number, "name", domain, block_height)
VALUES ('n01', 1, 'a.sats', 'sats', 100),
       ('n02', 2, 'ab.sats', 'sats', 101),
       ('n03', 3, 'ab.btc', 'btc', 102),
       ('n04', 4, 'abc.btc', 'btc', 103),
       ('n05', 5, 'a_b.sats', 'sats', 104),
       ('n06', 6, 'a%b.sats', 'sats', 105),
       ('n07', 7, 'a\b.sats', 'sats', 106),
       ('n08', 8, 'b.sats', 'sats', 107),
       ('n09', 9, 'école.x', 'x', 108),
       ('n10', 10, 'aς.1', '1', 109),
       ('n11', 11, 'aσb.x', 'x', 110),
       ('n12', 12, 'a.ς', 'ς', 111),
       ('n13', 13, 'ab_.x', 'x', 112);
"#;

async fn seeded<F, Fut>(tag: &str, collation: Option<&str>, body: F)
where
    F: FnOnce(Router, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_seeded(
        SNS,
        tag,
        collation,
        &[include_str!("../../opi-sns/src/db_init.sql"), FIXTURE],
        body,
    )
    .await;
}

fn ok(result: &str) -> String {
    format!(r#"{{"error":null,"result":{result}}}"#)
}

/// The names of a search answer, in order, with its `has_more` and
/// `next_after` (a name or None).
async fn names(app: &Router, query: &str) -> (Vec<String>, bool, Option<String>) {
    let uri = format!("/v1/sns/search?{query}");
    let answer = get(app, &uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
    let body = &answer.body;
    let listed = body
        .split(r#""name":"#)
        .skip(1)
        .map(|rest| rest[..rest.find(r#","namespace""#).unwrap()].to_string())
        .collect();
    let next_after = body
        .split(r#""next_after":"#)
        .nth(1)
        .map(|rest| rest[..rest.find(',').unwrap()].to_string())
        .filter(|next| next != "null");
    (listed, body.contains(r#""has_more":true"#), next_after)
}

/// `names` as the JSON strings of an answer.
fn json(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| js_string(name)).collect()
}

#[tokio::test]
async fn seeded_by_inscription_answers_the_name_or_404() {
    seeded("sns_insc", None, |app, _| async move {
        expect(
            &app,
            "/v1/sns/by_inscription?inscription_id=n02",
            200,
            JSON,
            &ok(
                r#"{"inscription_id":"n02","inscription_number":2,"name":"ab.sats","namespace":"sats","block_height":101}"#,
            ),
        )
        .await;
        expect(
            &app,
            "/v1/sns/by_inscription?inscription_id=n10",
            200,
            JSON,
            &ok(
                r#"{"inscription_id":"n10","inscription_number":10,"name":"aς.1","namespace":"1","block_height":109}"#,
            ),
        )
        .await;
        for id in ["nope", "N02", "n0", "n0_", "%25"] {
            expect(
                &app,
                &format!("/v1/sns/by_inscription?inscription_id={id}"),
                404,
                JSON,
                &bad("sns name not found"),
            )
            .await;
        }
    })
    .await;
}

/// The search checks shared by both collations: the order is bytewise.
async fn search_checks(app: Router) {
    // `%`, `_` and `\` sort before the letters in bytes
    let all_a = json(&[
        "a%b.sats",
        "a.sats",
        r"a\b.sats",
        "a_b.sats",
        "ab.btc",
        "ab.sats",
        "ab_.x",
        "abc.btc",
        "aς.1",
        "aσb.x",
        "a.ς",
    ]);
    // sorted bytewise: 'a.ς' < 'a.sats'? '.' (2e) < 's' (73) < 'ς' (cf 82)
    let mut expected = all_a.clone();
    expected.sort_by_key(|name| serde_unescape(name).into_bytes());
    assert_eq!(
        names(&app, "prefix=a").await,
        (expected.clone(), false, None)
    );
    // the prefix is lowercased the way the indexer lowercases names
    assert_eq!(names(&app, "prefix=A").await.0, expected);
    assert_eq!(
        names(&app, "prefix=AB").await.0,
        json(&["ab.btc", "ab.sats", "ab_.x", "abc.btc"])
    );
    assert_eq!(
        names(&app, &format!("prefix={}", encode("ÉCOLE"))).await.0,
        json(&["école.x"])
    );
    // wildcards match themselves
    assert_eq!(names(&app, "prefix=a_").await.0, json(&["a_b.sats"]));
    assert_eq!(names(&app, "prefix=a%25").await.0, json(&["a%b.sats"]));
    assert_eq!(names(&app, "prefix=a%5C").await.0, json(&[r"a\b.sats"]));
    assert_eq!(names(&app, "prefix=%25").await.0, json(&[]));
    assert_eq!(names(&app, "prefix=_").await.0, json(&[]));
    assert_eq!(names(&app, "prefix=ab_").await.0, json(&["ab_.x"]));
    // a name is a whole-prefix match from its first byte
    assert_eq!(names(&app, "prefix=b").await.0, json(&["b.sats"]));
    assert_eq!(names(&app, "prefix=sats").await.0, json(&[]));
    assert_eq!(names(&app, "prefix=a.sats").await.0, json(&["a.sats"]));
    // sigma: a prefix ending in a capital sigma finds the final and the
    // medial spelling
    assert_eq!(
        names(&app, &format!("prefix={}", encode("aΣ"))).await.0,
        json(&["aς.1", "aσb.x"])
    );
    assert_eq!(
        names(&app, &format!("prefix={}", encode("aΣB"))).await.0,
        json(&["aσb.x"])
    );
    // namespace filter, lowercased like the prefix
    assert_eq!(
        names(&app, "prefix=a&namespace=btc").await.0,
        json(&["ab.btc", "abc.btc"])
    );
    assert_eq!(
        names(&app, "prefix=a&namespace=BTC").await.0,
        json(&["ab.btc", "abc.btc"])
    );
    assert_eq!(names(&app, "prefix=a&namespace=nope").await.0, json(&[]));
    assert_eq!(
        names(&app, "prefix=a&namespace=").await.0.len(),
        expected.len()
    );
    // a namespace whose final sigma depends on the name it ends
    assert_eq!(
        names(&app, &format!("prefix=a&namespace={}", encode("Σ")))
            .await
            .0,
        json(&["a.ς"])
    );
    // whole answer
    expect(
        &app,
        "/v1/sns/search?prefix=B&namespace=SATS",
        200,
        JSON,
        &ok(
            r#"{"prefix":"b","namespace":"sats","limit":100,"has_more":false,"next_after":null,"names":[{"inscription_id":"n08","inscription_number":8,"name":"b.sats","namespace":"sats","block_height":107}]}"#,
        ),
    )
    .await;
}

/// The JSON string `text` as a Rust string (the fixture has no escapes but
/// the backslash's).
fn serde_unescape(text: &str) -> String {
    text.trim_matches('"').replace(r"\\", r"\")
}

async fn paging_checks(app: Router) {
    let (first, more, next) = names(&app, "prefix=a&limit=3").await;
    assert_eq!(first, json(&["a%b.sats", "a.sats", "a.ς"]));
    assert!(more);
    assert_eq!(next, Some(js_string("a.ς")));
    let cursor = encode("a.ς");
    let (second, more, next) = names(&app, &format!("prefix=a&limit=3&after={cursor}")).await;
    assert_eq!(second, json(&[r"a\b.sats", "a_b.sats", "ab.btc"]));
    assert!(more);
    assert_eq!(next, Some(js_string("ab.btc")));
    // a page of exactly the rest has no more
    let (rest, more, next) = names(&app, "prefix=ab&limit=4").await;
    assert_eq!(rest, json(&["ab.btc", "ab.sats", "ab_.x", "abc.btc"]));
    assert!(!more);
    assert_eq!(next, None);
    // the cursor need not be a listed name, and is not normalised or
    // matched against the prefix
    assert_eq!(
        names(&app, "prefix=ab&limit=2&after=ab.c").await.0,
        json(&["ab.sats", "ab_.x"])
    );
    assert_eq!(names(&app, "prefix=ab&after=zzz").await.0, json(&[]));
    // a filtered page cursor advances over names of other namespaces
    let (btc, more, next) = names(&app, "prefix=a&namespace=btc&limit=1").await;
    assert_eq!(btc, json(&["ab.btc"]));
    assert!(more);
    assert_eq!(next, Some(js_string("ab.btc")));
    assert_eq!(
        names(&app, "prefix=a&namespace=btc&limit=1&after=ab.btc").await,
        (json(&["abc.btc"]), false, None)
    );
}

#[tokio::test]
async fn seeded_search_matches_prefixes_and_normalises() {
    seeded("sns_search", None, |app, _| search_checks(app)).await;
}

#[tokio::test]
async fn seeded_search_pages_by_key() {
    seeded("sns_paging", None, |app, _| paging_checks(app)).await;
}

const BY_INSCRIPTIONS: &str = "/v1/sns/by_inscriptions";

/// The batch body of `ids`.
fn ids_body(ids: &[String]) -> String {
    serde_json::json!({ "inscription_ids": ids }).to_string()
}

/// The inscription id whose txid is `byte` 32 times, with index `index`.
fn inscription(byte: &str, index: u32) -> String {
    format!("{}i{index}", byte.repeat(32))
}

#[tokio::test]
async fn by_inscriptions_refuses_a_bad_body_before_any_query() {
    let app = height(SNS, 7.into());
    let shape = "the body must be JSON {\"inscription_ids\":[...]} with 1 to 500 inscription ids";
    let item = "inscription_ids[1] must be an inscription id <txid>i<index> (64 hex digits, `i` and a 32-bit index)";
    let one = inscription("ab", 0);
    for (body, reason) in [
        (String::new(), shape),
        ("not json".into(), shape),
        (r#"{"inscription_ids":"x"}"#.into(), shape),
        (ids_body(&[]), shape),
        (ids_body(&vec![one.clone(); 501]), shape),
        (ids_body(&[one.clone(), "n02".into()]), item),
    ] {
        let answer = post(&app, BY_INSCRIPTIONS, &body).await;
        assert_eq!(answer.status, 400, "{body}");
        let refusal = serde_json::json!({"error": reason, "result": null}).to_string();
        assert_eq!(answer.body, refusal, "{body}");
    }
    let answer = post(&app, BY_INSCRIPTIONS, &ids_body(&[one])).await;
    assert_eq!((answer.status, answer.body.as_str()), (500, INTERNAL_ERROR));
}

/// Names under canonical inscription ids: `aa…i0` registered one, `bb…i3`
/// two rows (the lowest id answers), `cc…i0` none; and the module's blocks.
const CANONICAL_NAMES: &str = r#"
INSERT INTO sns_block_hashes (block_height, block_hash) VALUES (200, 'h200'), (201, 'h201');
INSERT INTO sns_names (id, inscription_id, inscription_number, "name", domain, block_height)
VALUES (100, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaai0', 20, 'x.sats', 'sats', 200),
       (102, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbi3', 22, 'z.btc', 'btc', 201),
       (101, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbi3', 21, 'y.btc', 'btc', 201);
"#;

#[tokio::test]
async fn seeded_by_inscriptions_answer_each_id_as_the_single_route() {
    seeded("sns_batch", None, |app, pool| async move {
        sqlx::raw_sql(CANONICAL_NAMES).execute(&pool).await.unwrap();
        let (a, b, c) = (
            inscription("aa", 0),
            inscription("bb", 3),
            inscription("cc", 0),
        );
        let requested = [
            format!("{}i03", "BB".repeat(32)),
            c.clone(),
            a.clone(),
            b.clone(),
        ];
        let answer = post(&app, BY_INSCRIPTIONS, &ids_body(&requested)).await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some(JSON));
        let name = |id: &str, number: u32, name: &str, namespace: &str, height: u32| {
            format!(
                r#"{{"inscription_id":"{id}","inscription_number":{number},"name":"{name}","namespace":"{namespace}","block_height":{height}}}"#
            )
        };
        let (name_a, name_b) = (
            name(&a, 20, "x.sats", "sats", 200),
            name(&b, 21, "y.btc", "btc", 201),
        );
        assert_eq!(
            answer.body,
            ok(&format!(
                r#"{{"names":[{{"inscription_id":"{b}","name":{name_b}}},{{"inscription_id":"{c}","name":null}},{{"inscription_id":"{a}","name":{name_a}}},{{"inscription_id":"{b}","name":{name_b}}}],"block_height":201}}"#
            ))
        );
        // each name is what by_inscription answers
        for (id, name) in [(&a, &name_a), (&b, &name_b)] {
            expect(
                &app,
                &format!("/v1/sns/by_inscription?inscription_id={id}"),
                200,
                JSON,
                &ok(name),
            )
            .await;
        }
        expect(
            &app,
            &format!("/v1/sns/by_inscription?inscription_id={c}"),
            404,
            JSON,
            &bad("sns name not found"),
        )
        .await;
    })
    .await;
}

/// The order and the cursor are bytewise whatever the database collation.
#[tokio::test]
async fn seeded_search_is_bytewise_in_a_locale_collation() {
    seeded("sns_locale", Some("en_US.UTF-8"), |app, _| async move {
        search_checks(app.clone()).await;
        paging_checks(app).await;
    })
    .await;
}

/// Explain of the search in a locale-collated database, where only the `C`
/// index serves it: the prefix range and the order read that index, with no
/// sort (under forced index plans, as the table is tiny).
#[tokio::test]
async fn seeded_search_plans_use_the_c_collation_index() {
    seeded("sns_plan", Some("en_US.UTF-8"), |_, pool| async move {
        let mut conn = pool.acquire().await.unwrap();
        for setting in ["SET enable_seqscan = off", "SET enable_bitmapscan = off"] {
            sqlx::query(setting).execute(&mut *conn).await.unwrap();
        }
        let plan: Vec<String> = sqlx::query_scalar(
            r#"EXPLAIN SELECT "name" FROM sns_names
               WHERE "name" COLLATE "C" LIKE 'ab%' AND "name" COLLATE "C" > 'ab.c'
               ORDER BY "name" COLLATE "C" LIMIT 11"#,
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains("sns_names_name_c_idx"), "{plan}");
        assert!(!plan.contains("Sort"), "{plan}");
    })
    .await;
}
