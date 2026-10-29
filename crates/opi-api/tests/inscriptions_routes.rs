//! The inscriptions routes over HTTP: `/v1/inscriptions/inscription`,
//! `/v1/inscriptions/wallet`, `/v1/inscriptions/outpoint`,
//! `/v1/inscriptions/outpoints`, `/v1/outputs` and the inscriptions blocks
//! of `/v1/stream`.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters and bodies are checked before any query, and a valid
//! request answers the 500 body when its query fails. The seeded tests build
//! a throwaway database from opi-inscriptions's db_init.sql (and
//! opi-runes's, for `/v1/outputs`) on the local Postgres and skip unless
//! `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{
        INTERNAL_ERROR, JSON, Sse, encode, expect, get, height, modules_app, post,
        with_seeded_modules,
    },
    opi_api::{config::Module, db::Db},
    serde_json::{Value, json},
    sqlx::PgPool,
};

const INSCRIPTIONS: Module = Module::Inscriptions;

fn bad(reason: &str) -> String {
    json!({"error": reason, "result": null}).to_string()
}

/// The txid of `n`: 64 hex digits.
fn tx(n: u32) -> String {
    format!("{n:064x}")
}

fn outpoint(n: u32, vout: u32) -> String {
    format!("{}:{vout}", tx(n))
}

/// The inscription id of sequence number `seq`.
fn id(seq: u32) -> String {
    format!("{}i0", tx(1000 + seq))
}

/// The body of a batch request.
fn batch(outpoints: &[String]) -> String {
    json!({ "outpoints": outpoints }).to_string()
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(INSCRIPTIONS, 7.into());
    let id_reason =
        "id must be an inscription id <txid>i<index> (64 hex digits, `i` and a 32-bit index)";
    let number = "number must be an integer from -2147483648 to 2147483647";
    let outpoint_reason = "outpoint must be an outpoint <txid>:<vout> (64 hex digits, a colon and a 32-bit output index)";
    let txid = tx(1);
    for (uri, reason) in [
        (
            "/v1/inscriptions/inscription".to_string(),
            "id or number is required",
        ),
        (
            format!("/v1/inscriptions/inscription?id={txid}i0&number=1"),
            "give id or number, not both",
        ),
        (format!("/v1/inscriptions/inscription?id={txid}"), id_reason),
        (
            format!("/v1/inscriptions/inscription?id={txid}i-1"),
            id_reason,
        ),
        (
            format!("/v1/inscriptions/inscription?id={txid}:0"),
            id_reason,
        ),
        ("/v1/inscriptions/inscription?number=1x".into(), number),
        (
            "/v1/inscriptions/inscription?number=2147483648".into(),
            number,
        ),
        (
            "/v1/inscriptions/inscription?number=1&number=2".into(),
            "number must be a single value",
        ),
        (
            "/v1/inscriptions/wallet".into(),
            "address or pkscript is required",
        ),
        (
            "/v1/inscriptions/wallet?address=a&pkscript=00".into(),
            "give address or pkscript, not both",
        ),
        (
            "/v1/inscriptions/wallet?pkscript=0".into(),
            "pkscript must be hex of at most 20000 digits",
        ),
        (
            "/v1/inscriptions/wallet?address=a&content_type=%3B".into(),
            "content_type must be a media type (text/plain) or a prefix ending with / (image/)",
        ),
        (
            format!(
                "/v1/inscriptions/wallet?address=a&content_type={}",
                "a".repeat(257)
            ),
            "content_type must be at most 256 bytes without NUL",
        ),
        (
            "/v1/inscriptions/wallet?address=a&cursed=1".into(),
            "cursed must be true or false",
        ),
        (
            "/v1/inscriptions/wallet?address=a&from_height=x".into(),
            "from_height must be an integer from 0 to 2147483647",
        ),
        (
            "/v1/inscriptions/wallet?address=a&from_height=5&to_height=4".into(),
            "from_height must not be above to_height",
        ),
        (
            "/v1/inscriptions/wallet?address=a&after=1.5".into(),
            "after must be the next_after of a previous page",
        ),
        (
            "/v1/inscriptions/wallet?address=a&limit=501".into(),
            "limit must be an integer from 1 to 500",
        ),
        ("/v1/inscriptions/outpoint".into(), "outpoint is required"),
        (
            format!("/v1/inscriptions/outpoint?outpoint={txid}"),
            outpoint_reason,
        ),
        (
            format!("/v1/inscriptions/outpoint?outpoint={txid}:4294967296"),
            outpoint_reason,
        ),
        (
            "/v1/inscriptions/outpoint?outpoint=x:0".into(),
            outpoint_reason,
        ),
    ] {
        expect(&app, &uri, 400, JSON, &bad(reason)).await;
    }
    let shape = "the body must be JSON {\"outpoints\":[...]} with 1 to 500 outpoints";
    let one = outpoint(1, 0);
    for (body, reason) in [
        (String::new(), shape.to_string()),
        ("{".into(), shape.into()),
        (json!([one]).to_string(), shape.into()),
        (batch(&[]), shape.into()),
        (batch(&vec![one.clone(); 501]), shape.into()),
        (
            batch(&[one.clone(), "x".into()]),
            "outpoints[1] must be an outpoint <txid>:<vout> (64 hex digits, a colon and a 32-bit output index)".into(),
        ),
    ] {
        let answer = post(&app, "/v1/inscriptions/outpoints", &body).await;
        assert_eq!(answer.status, 400, "{body}");
        assert_eq!(answer.body, bad(&reason), "{body}");
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(INSCRIPTIONS, 7.into());
    let txid = tx(1);
    for uri in [
        format!("/v1/inscriptions/inscription?id={txid}i0"),
        "/v1/inscriptions/inscription?number=-3".into(),
        "/v1/inscriptions/wallet?address=tb1q&content_type=image/".into(),
        format!("/v1/inscriptions/outpoint?outpoint={txid}:0"),
    ] {
        expect(&app, &uri, 500, JSON, INTERNAL_ERROR).await;
    }
    let answer = post(
        &app,
        "/v1/inscriptions/outpoints",
        &batch(&[outpoint(1, 0)]),
    )
    .await;
    assert_eq!((answer.status, answer.body.as_str()), (500, INTERNAL_ERROR));
}

/// `/v1/outputs` answers only with both inscriptions and runes enabled.
#[tokio::test]
async fn outputs_refuses_without_both_modules() {
    let fake = || Db::fake(|_| Ok(7.into()));
    let body = batch(&[outpoint(1, 0)]);
    for (modules, missing) in [
        (&[Module::Inscriptions][..], "runes"),
        (&[Module::Runes][..], "inscriptions"),
        (&[Module::Brc20][..], "inscriptions"),
    ] {
        let app = modules_app(modules, fake(), &[]);
        let answer = post(&app, "/v1/outputs", &body).await;
        assert_eq!(answer.status, 404, "{modules:?}");
        assert_eq!(
            answer.body,
            bad(&format!("{missing} is not enabled on this node")),
            "{modules:?}"
        );
    }
    let app = modules_app(&[Module::Inscriptions, Module::Runes], fake(), &[]);
    let answer = post(&app, "/v1/outputs", &batch(&[])).await;
    assert_eq!(answer.status, 400);
    let answer = post(&app, "/v1/outputs", &body).await;
    assert_eq!((answer.status, answer.body.as_str()), (500, INTERNAL_ERROR));
}

/// An entry of the fixture: (sequence number, number, content type, media
/// type, outpoint, offset, wallet, genesis height, last moved height).
type EntryFixture = (
    u32,
    i64,
    Option<&'static str>,
    Option<&'static str>,
    String,
    i64,
    char,
    i32,
    i32,
);

/// Fifteen inscriptions: sequence numbers 0 to 11 in wallet A (`addrA`,
/// pkScript `0014aa`), 12 and 13 in wallet B (pkScript `0014bb`, no
/// address), 14 burned. 1 and 2 share an output at offsets 0 and 546; 4 and
/// 9 are cursed. Blocks 1 to 20 are indexed; blocks 1 to 4 hold events.
fn inscriptions_fixture() -> String {
    let rows: [EntryFixture; 15] = [
        (
            0,
            0,
            Some("text/plain"),
            Some("text/plain"),
            outpoint(200, 0),
            0,
            'A',
            1,
            1,
        ),
        (
            1,
            1,
            Some("image/png"),
            Some("image/png"),
            outpoint(100, 0),
            0,
            'A',
            2,
            3,
        ),
        (
            2,
            2,
            Some("image/webp"),
            Some("image/webp"),
            outpoint(100, 0),
            546,
            'A',
            3,
            3,
        ),
        (
            3,
            3,
            Some("text/html;charset=utf-8"),
            Some("text/html"),
            outpoint(100, 1),
            0,
            'A',
            4,
            4,
        ),
        (
            4,
            -1,
            Some("image/png"),
            Some("image/png"),
            outpoint(204, 0),
            0,
            'A',
            5,
            5,
        ),
        (
            5,
            4,
            Some("application/json"),
            Some("application/json"),
            outpoint(205, 0),
            0,
            'A',
            6,
            6,
        ),
        (6, 5, None, None, outpoint(206, 0), 0, 'A', 7, 7),
        (
            7,
            6,
            Some("image/svg+xml"),
            Some("image/svg+xml"),
            outpoint(207, 0),
            0,
            'A',
            8,
            8,
        ),
        (
            8,
            7,
            Some("Text/Plain;charset=UTF-8"),
            Some("text/plain"),
            outpoint(208, 0),
            0,
            'A',
            9,
            9,
        ),
        (
            9,
            -2,
            Some("image/gif"),
            Some("image/gif"),
            outpoint(209, 0),
            0,
            'A',
            10,
            10,
        ),
        (
            10,
            8,
            Some("imagex/foo"),
            Some("imagex/foo"),
            outpoint(210, 0),
            0,
            'A',
            11,
            11,
        ),
        (
            11,
            9,
            Some("im_ge/x"),
            Some("im_ge/x"),
            outpoint(211, 0),
            0,
            'A',
            12,
            12,
        ),
        (
            12,
            10,
            Some("text/plain"),
            Some("text/plain"),
            outpoint(212, 0),
            0,
            'B',
            13,
            13,
        ),
        (
            13,
            11,
            Some("image/png"),
            Some("image/png"),
            outpoint(213, 0),
            0,
            'B',
            14,
            14,
        ),
        (
            14,
            12,
            Some("text/plain"),
            Some("text/plain"),
            outpoint(214, 0),
            0,
            'X',
            15,
            15,
        ),
    ];
    let text = |value: Option<&str>| value.map_or("NULL".to_string(), |v| format!("'{v}'"));
    let values: Vec<String> = rows
        .iter()
        .map(
            |(seq, number, content_type, media_type, outpoint, offset, wallet, genesis, moved)| {
                let (pkscript, address, charms) = match wallet {
                    'A' => ("0014aa", "'addrA'", 0),
                    'B' => ("0014bb", "NULL", 0),
                    _ => ("6a", "NULL", 4096),
                };
                let charms = if *number < 0 { charms | 2 } else { charms };
                format!(
                    "({seq}, '{}', {number}, {}, {}, {charms}, '{outpoint}', {offset}, '{pkscript}', {address}, {genesis}, {moved})",
                    id(*seq),
                    text(*content_type),
                    text(*media_type),
                )
            },
        )
        .collect();
    let event = |id: u32, height: i32, seq: u32, old: Option<&str>, new: &str| {
        let old = old.map_or("NULL, NULL, NULL, NULL, NULL".to_string(), |old| {
            format!("'{old}', 0, '0014cc', 'addrC', 2")
        });
        format!(
            "({id}, {height}, {seq}, '{}', {old}, '{new}', 0, '0014aa', 'addrA')",
            self::id(seq)
        )
    };
    let events = [
        event(1, 1, 0, None, &outpoint(200, 0)),
        event(2, 2, 1, None, &outpoint(150, 0)),
        event(3, 3, 2, None, &outpoint(100, 0)),
        event(4, 3, 1, Some(&outpoint(150, 0)), &outpoint(100, 0)),
        event(5, 4, 3, None, &outpoint(100, 1)),
    ];
    format!(
        "INSERT INTO inscriptions_block_hashes (block_height, block_hash, block_time)
             SELECT h, 'b' || h, to_timestamp(1700000000 + h) FROM generate_series(1, 20) h;
         INSERT INTO inscriptions_entries (sequence_number, inscription_id, \"number\", content_type,
             media_type, charms, outpoint, \"offset\", pkscript, address, genesis_height,
             last_moved_height) VALUES {};
         INSERT INTO inscriptions_events (id, block_height, sequence_number, inscription_id,
             old_outpoint, old_offset, old_pkscript, old_address, old_last_moved_height,
             new_outpoint, new_offset, new_pkscript, new_address) VALUES {};",
        values.join(",\n"),
        events.join(",\n"),
    )
}

/// Runes at height 18: rune `5:1` (TEST•RUNE, divisibility 2) on the
/// inscribed output `100:1` and, with the unlisted rune `6:1`, on `300:0`;
/// `301:0` held runes and is spent.
fn runes_fixture() -> String {
    format!(
        "INSERT INTO runes_block_hashes (block_height, block_hash, block_time)
             SELECT h, 'b' || h, to_timestamp(1700000000 + h) FROM generate_series(1, 18) h;
         INSERT INTO runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching, mints,
             \"number\", premine, rune_name, spacers, symbol, \"timestamp\", turbo, genesis_height,
             last_updated_block_height)
             VALUES ('5:1', 5, 0, 2, repeat('e', 64), 0, 0, 1000, 'TESTRUNE', 8, NULL,
                 to_timestamp(0), false, 5, 5);
         INSERT INTO runes_outpoint_to_balances (outpoint, pkscript, wallet_addr, rune_ids, balances,
             block_height, spent, spent_block_height) VALUES
             ('{}', '0014aa', 'addrA', '{{5:1}}', '{{150}}', 4, false, NULL),
             ('{}', '0014aa', 'addrA', '{{5:1,6:1}}', '{{7,9}}', 5, false, NULL),
             ('{}', '0014aa', 'addrA', '{{5:1}}', '{{1}}', 5, true, 6);",
        outpoint(100, 1),
        outpoint(300, 0),
        outpoint(301, 0),
    )
}

/// Runs `body` with the app of `modules` on a throwaway database seeded
/// with the inscriptions and runes fixtures.
async fn with_fixture<F, Fut>(tag: &str, modules: &[Module], body: F)
where
    F: FnOnce(Router, PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let inscriptions = inscriptions_fixture();
    let runes = runes_fixture();
    with_seeded_modules(
        modules,
        tag,
        None,
        &[
            include_str!("../../opi-inscriptions/src/db_init.sql"),
            include_str!("../../opi-runes/src/db_init.sql"),
            &inscriptions,
            &runes,
        ],
        4,
        body,
    )
    .await;
}

async fn result(app: &Router, uri: &str) -> Value {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["error"], Value::Null, "{uri}");
    body["result"].clone()
}

async fn post_result(app: &Router, uri: &str, body: &str) -> Value {
    let answer = post(app, uri, body).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["error"], Value::Null, "{uri}");
    body["result"].clone()
}

fn inscription_1() -> Value {
    json!({
        "inscription_id": id(1),
        "number": 1,
        "sequence_number": 1,
        "content_type": "image/png",
        "media_type": "image/png",
        "charms": 0,
        "cursed": false,
        "outpoint": outpoint(100, 0),
        "offset": 0,
        "pkscript": "0014aa",
        "address": "addrA",
        "genesis_height": 2,
        "last_moved_height": 3,
    })
}

#[tokio::test]
async fn an_inscription_answers_by_id_or_number() {
    with_fixture("inscription", &[INSCRIPTIONS], |app, _| async move {
        let mut expected = inscription_1();
        expected["indexed_height"] = json!(20);
        let upper = id(1).to_ascii_uppercase().replace('I', "i");
        for uri in [
            format!("/v1/inscriptions/inscription?id={}", id(1)),
            format!("/v1/inscriptions/inscription?id={upper}"),
            format!("/v1/inscriptions/inscription?id={}i00", tx(1001)),
            "/v1/inscriptions/inscription?number=1".into(),
        ] {
            assert_eq!(result(&app, &uri).await, expected, "{uri}");
        }
        let cursed = result(&app, "/v1/inscriptions/inscription?number=-2").await;
        assert_eq!(cursed["inscription_id"], id(9));
        assert_eq!(cursed["cursed"], true);
        assert_eq!(cursed["charms"], 2);
        let burned = result(&app, &format!("/v1/inscriptions/inscription?id={}", id(14))).await;
        assert_eq!(
            (&burned["pkscript"], &burned["address"]),
            (&json!("6a"), &Value::Null)
        );
        let untyped = result(&app, "/v1/inscriptions/inscription?number=5").await;
        assert_eq!(
            (&untyped["content_type"], &untyped["media_type"]),
            (&Value::Null, &Value::Null)
        );
        let missing = bad("inscription not found");
        for uri in [
            format!("/v1/inscriptions/inscription?id={}", id(99)),
            "/v1/inscriptions/inscription?number=-99".into(),
        ] {
            expect(&app, &uri, 404, JSON, &missing).await;
        }
    })
    .await;
}

/// Every page of `first` with `limit=2`, following `next_after`, as the
/// sequence numbers of each page.
async fn wallet_pages(app: &Router, first: &str) -> Vec<Vec<i64>> {
    let first = format!("{first}&limit=2");
    let mut uri = first.clone();
    let mut pages = Vec::new();
    loop {
        let page = result(app, &uri).await;
        assert_eq!(page["limit"], 2, "{uri}");
        assert_eq!(page["indexed_height"], 20, "{uri}");
        let rows = page["inscriptions"].as_array().unwrap();
        assert!(rows.len() <= 2, "{uri}");
        pages.push(
            rows.iter()
                .map(|row| row["sequence_number"].as_i64().unwrap())
                .collect(),
        );
        if !page["has_more"].as_bool().unwrap() {
            assert_eq!(page["next_after"], Value::Null, "{uri}");
            return pages;
        }
        let next = page["next_after"].as_str().unwrap();
        uri = format!("{first}&after={}", encode(next));
    }
}

/// A wallet's pages, newest first, hold exactly the rows a SQL query with
/// the same filter counts.
#[tokio::test]
async fn a_wallet_pages_newest_first_and_filters_match_a_sql_count() {
    with_fixture("wallet", &[INSCRIPTIONS], |app, pool| async move {
        for (wallet, wallet_sql) in [
            ("address=addrA", "address = 'addrA'"),
            ("pkscript=0014AA", "pkscript = '0014aa'"),
            ("pkscript=0014bb", "pkscript = '0014bb'"),
            ("address=nobody", "address = 'nobody'"),
        ] {
            for (filter, filter_sql) in [
                ("", "true"),
                ("&content_type=text/plain", "media_type = 'text/plain'"),
                (
                    "&content_type=TEXT/Plain%3B%20charset%3Dutf-8",
                    "media_type = 'text/plain'",
                ),
                ("&content_type=image/", "media_type like 'image/%'"),
                ("&content_type=IMAGE/", "media_type like 'image/%'"),
                ("&content_type=im_ge/", "media_type like 'im\\_ge/%'"),
                ("&content_type=image/png", "media_type = 'image/png'"),
                ("&cursed=true", "\"number\" < 0"),
                ("&cursed=false", "\"number\" >= 0"),
                (
                    "&from_height=4&to_height=9",
                    "genesis_height between 4 and 9",
                ),
                ("&from_height=10", "genesis_height >= 10"),
                (
                    "&content_type=image/&cursed=false&to_height=8",
                    "media_type like 'image/%' and \"number\" >= 0 and genesis_height <= 8",
                ),
            ] {
                let uri = format!("/v1/inscriptions/wallet?{wallet}{filter}");
                let pages = wallet_pages(&app, &uri).await;
                let listed: Vec<i64> = pages.concat();
                let expected: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "select sequence_number from inscriptions_entries
                         where {wallet_sql} and {filter_sql}
                         order by sequence_number desc"
                )))
                .fetch_all(&pool)
                .await
                .unwrap();
                assert_eq!(listed, expected, "{uri}");
                let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "select count(*) from inscriptions_entries where {wallet_sql} and {filter_sql}"
                )))
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(listed.len() as i64, count, "{uri}");
            }
        }
        // the filters as the answer applied them
        let page = result(
            &app,
            "/v1/inscriptions/wallet?pkscript=0014AA&content_type=Image/&cursed=true&from_height=3",
        )
        .await;
        assert_eq!(page["address"], Value::Null);
        assert_eq!(page["pkscript"], "0014aa");
        assert_eq!(page["content_type"], "image/");
        assert_eq!(page["cursed"], true);
        assert_eq!(page["from_height"], 3);
        assert_eq!(page["to_height"], Value::Null);
        let ids: Vec<&Value> = page["inscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| &row["inscription_id"])
            .collect();
        assert_eq!(ids, [&json!(id(9)), &json!(id(4))]);
        // a full row, as /v1/inscriptions/inscription answers it
        let page = result(
            &app,
            "/v1/inscriptions/wallet?address=addrA&content_type=image/png&cursed=false",
        )
        .await;
        assert_eq!(page["inscriptions"], json!([inscription_1()]));
        assert_eq!(page["has_more"], false);
    })
    .await;
}

#[tokio::test]
async fn an_outpoint_lists_its_inscriptions_by_offset() {
    with_fixture("outpoint", &[INSCRIPTIONS], |app, _| async move {
        let shared = result(
            &app,
            &format!(
                "/v1/inscriptions/outpoint?outpoint={}:00",
                tx(100).to_ascii_uppercase()
            ),
        )
        .await;
        assert_eq!(shared["outpoint"], outpoint(100, 0));
        assert_eq!(shared["indexed_height"], 20);
        let rows = shared["inscriptions"].as_array().unwrap();
        assert_eq!(rows[0], inscription_1());
        assert_eq!(
            rows.iter()
                .map(|row| (&row["sequence_number"], &row["offset"]))
                .collect::<Vec<_>>(),
            [(&json!(1), &json!(0)), (&json!(2), &json!(546))]
        );
        let clean = result(
            &app,
            &format!("/v1/inscriptions/outpoint?outpoint={}", outpoint(150, 0)),
        )
        .await;
        assert_eq!(
            clean,
            json!({"outpoint": outpoint(150, 0), "inscriptions": [], "indexed_height": 20})
        );
    })
    .await;
}

#[tokio::test]
async fn outpoints_answer_each_requested_output_in_order() {
    with_fixture("outpoints", &[INSCRIPTIONS], |app, _| async move {
        let requested = [
            outpoint(150, 0),
            format!("{}:0", tx(100).to_ascii_uppercase()),
            outpoint(214, 0),
            outpoint(100, 0),
        ];
        let answer = post_result(&app, "/v1/inscriptions/outpoints", &batch(&requested)).await;
        assert_eq!(answer["indexed_height"], 20);
        let entries = answer["outpoints"].as_array().unwrap();
        let summary: Vec<(String, Vec<i64>)> = entries
            .iter()
            .map(|entry| {
                (
                    entry["outpoint"].as_str().unwrap().to_string(),
                    entry["inscriptions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["sequence_number"].as_i64().unwrap())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (outpoint(150, 0), vec![]),
                (outpoint(100, 0), vec![1, 2]),
                (outpoint(214, 0), vec![14]),
                (outpoint(100, 0), vec![1, 2]),
            ]
        );
        assert_eq!(entries[1]["inscriptions"][0], inscription_1());
        // the cap is 500 outpoints
        let many: Vec<String> = (0..500).map(|n| outpoint(5000 + n, 0)).collect();
        let answer = post_result(&app, "/v1/inscriptions/outpoints", &batch(&many)).await;
        assert_eq!(answer["outpoints"].as_array().unwrap().len(), 500);
        let mut too_many = many;
        too_many.push(outpoint(100, 0));
        let answer = post(&app, "/v1/inscriptions/outpoints", &batch(&too_many)).await;
        assert_eq!(answer.status, 400);
    })
    .await;
}

/// `/v1/outputs` marks every inscribed and runic output, and nothing else,
/// at the lower of the two modules' heights.
#[tokio::test]
async fn outputs_marks_inscribed_and_runic_outputs() {
    with_fixture(
        "outputs",
        &[Module::Inscriptions, Module::Runes],
        |app, _| async move {
            let requested = [
                outpoint(100, 1),
                outpoint(300, 0),
                outpoint(301, 0),
                outpoint(150, 0),
                outpoint(100, 0),
            ];
            let answer = post_result(&app, "/v1/outputs", &batch(&requested)).await;
            assert_eq!(answer["indexed_height"], 18);
            assert_eq!(answer["heights"], json!({"inscriptions": 20, "runes": 18}));
            let outputs = answer["outputs"].as_array().unwrap();
            let rune = |id: &str, amount: &str| {
                if id == "5:1" {
                    json!({"rune_id": "5:1", "rune_name": "TESTRUNE",
                        "spaced_rune_name": "TEST•RUNE", "divisibility": 2, "symbol": null,
                        "amount": amount})
                } else {
                    json!({"rune_id": id, "rune_name": null, "spaced_rune_name": null,
                        "divisibility": null, "symbol": null, "amount": amount})
                }
            };
            assert_eq!(outputs[0]["outpoint"], outpoint(100, 1));
            assert_eq!(outputs[0]["inscriptions"][0]["inscription_id"], id(3));
            assert_eq!(outputs[0]["runes"], json!([rune("5:1", "150")]));
            assert_eq!(outputs[1]["inscriptions"], json!([]));
            assert_eq!(
                outputs[1]["runes"],
                json!([rune("5:1", "7"), rune("6:1", "9")])
            );
            // a spent runic output and a clean one hold nothing
            for output in &outputs[2..4] {
                assert_eq!(output["inscriptions"], json!([]), "{output}");
                assert_eq!(output["runes"], json!([]), "{output}");
            }
            assert_eq!(outputs[4]["inscriptions"].as_array().unwrap().len(), 2);
            assert_eq!(outputs[4]["runes"], json!([]));
            let answer = post(&app, "/v1/outputs", &batch(&vec![outpoint(1, 0); 501])).await;
            assert_eq!(answer.status, 400);
        },
    )
    .await;
}

/// The stream carries each inscriptions block's creations and transfers,
/// and rolls back a replaced block.
#[tokio::test]
async fn the_stream_carries_creations_and_transfers() {
    with_fixture("stream", &[INSCRIPTIONS], |_, pool| async move {
        let app = modules_app(
            &[INSCRIPTIONS],
            Db::from_pool(pool.clone()),
            &[("STREAM_POLL_MS", "50")],
        );
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", None).await;
        let mut blocks = Vec::new();
        for height in 1..=20 {
            let frame = sse.next_message().await.expect("a block");
            assert_eq!(frame.event, "block", "{frame:?}");
            assert_eq!(frame.data["module"], "inscriptions");
            assert_eq!(frame.data["height"], height);
            assert_eq!(frame.data["hash"], format!("b{height}"));
            assert_eq!(frame.data["block_time"], 1_700_000_000 + height);
            assert_eq!(
                frame.id.as_deref(),
                Some(format!("inscriptions:{height}:b{height}").as_str())
            );
            blocks.push(frame.data["events"].clone());
        }
        assert_eq!(
            blocks[0],
            json!([{
                "id": 1, "block_height": 1, "event_type": "creation", "sequence_number": 0,
                "inscription_id": id(0), "number": 0, "cursed": false,
                "content_type": "text/plain", "media_type": "text/plain",
                "old_outpoint": null, "old_offset": null, "old_pkscript": null,
                "old_address": null, "new_outpoint": outpoint(200, 0), "new_offset": 0,
                "new_pkscript": "0014aa", "new_address": "addrA",
            }])
        );
        let block_3: Vec<(&Value, &Value, &Value)> = blocks[2]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| (&event["event_type"], &event["sequence_number"], &event["old_outpoint"]))
            .collect();
        assert_eq!(
            block_3,
            [
                (&json!("creation"), &json!(2), &Value::Null),
                (&json!("transfer"), &json!(1), &json!(outpoint(150, 0))),
            ]
        );
        assert_eq!(blocks[2][1]["old_address"], "addrC");
        assert_eq!(blocks[2][1]["new_outpoint"], outpoint(100, 0));
        assert!(blocks[4..].iter().all(|events| events == &json!([])));
        // a reorg replaces blocks 19 and 20
        sqlx::raw_sql(
            "BEGIN;
             DELETE FROM inscriptions_events WHERE block_height > 18;
             DELETE FROM inscriptions_block_hashes WHERE block_height > 18;
             INSERT INTO inscriptions_block_hashes (block_height, block_hash, block_time)
                 VALUES (19, 'c19', to_timestamp(1700000019)), (20, 'c20', to_timestamp(1700000020));
             COMMIT;",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rollback = sse.next_message().await.expect("a rollback");
        assert_eq!(rollback.event, "rollback");
        assert_eq!(
            rollback.data,
            json!({"module": "inscriptions", "height": 18})
        );
        for height in [19, 20] {
            let frame = sse.next_message().await.expect("a block");
            assert_eq!(frame.data["hash"], format!("c{height}"));
        }
    })
    .await;
}
