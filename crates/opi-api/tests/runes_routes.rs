//! The rune routes over HTTP: `/v1/runes/rune`, `/v1/runes/runes`,
//! `/v1/runes/rune/holders`, `/v1/runes/wallet/balances`,
//! `/v1/runes/rune/events` and `/v1/runes/wallet/events`.
//!
//! The first tests use a database double that answers only the latest block
//! height: parameters are checked before any query, and a valid request
//! answers the 500 body when its query fails. The seeded tests build a
//! throwaway database from opi-runes's db_init.sql, fill it with entries,
//! outpoints and events, drop the holder tables and let the runes module's
//! start-up fill rebuild them; they skip unless `OPI_TEST_PG=1`. The live
//! test reads OPI_API_TEST_RUNES_URL (a runes database with the holder
//! tables, read only) and is skipped when it is unset.

mod common;

use {
    axum::Router,
    common::{INTERNAL_ERROR, JSON, encode, expect, get, height, live, with_seeded},
    opi_api::config::Module,
    serde_json::{Value, json},
    sqlx::PgPool,
    std::collections::BTreeSet,
};

const RUNES: Module = Module::Runes;
const NOT_FOUND: &str = r#"{"error":"rune not found","result":null}"#;

fn bad(reason: &str) -> String {
    json!({"error": reason, "result": null}).to_string()
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(RUNES, 7.into());
    let rune = "rune must be a rune id (block:tx) or a rune name (letters A-Z and spacers)";
    let after = "after must be the next_after of a previous page";
    let long = "A".repeat(129);
    for (uri, reason) in [
        ("/v1/runes/rune".to_string(), "rune is required"),
        ("/v1/runes/rune?rune=".into(), "rune is required"),
        (
            "/v1/runes/rune?rune=a&rune=b".into(),
            "rune must be a single value",
        ),
        (
            "/v1/runes/rune?rune=A%00".into(),
            "rune must be at most 128 bytes without NUL",
        ),
        (
            format!("/v1/runes/rune?rune={long}"),
            "rune must be at most 128 bytes without NUL",
        ),
        ("/v1/runes/rune?rune=A..B".into(), rune),
        ("/v1/runes/rune?rune=1%3Ax".into(), rune),
        ("/v1/runes/rune?rune=ABC1".into(), rune),
        (
            "/v1/runes/runes?sort=name".into(),
            "sort must be one of holders, supply, mint_progress, etching_height",
        ),
        (
            "/v1/runes/runes?order=up".into(),
            "order must be asc or desc",
        ),
        (
            "/v1/runes/runes?search=A-B".into(),
            "search must be rune name letters A-Z, with spacers between",
        ),
        (
            "/v1/runes/runes?search=%00".into(),
            "search must be at most 128 bytes without NUL",
        ),
        ("/v1/runes/runes?after=7".into(), after),
        (
            "/v1/runes/runes?limit=0".into(),
            "limit must be an integer from 1 to 500",
        ),
        (
            "/v1/runes/runes?limit=501".into(),
            "limit must be an integer from 1 to 500",
        ),
        ("/v1/runes/rune/holders?rune=A&after=x%3A00".into(), after),
        ("/v1/runes/rune/holders".into(), "rune is required"),
        (
            "/v1/runes/wallet/balances".into(),
            "address or pkscript is required",
        ),
        (
            "/v1/runes/wallet/balances?address=a&pkscript=00".into(),
            "give address or pkscript, not both",
        ),
        (
            "/v1/runes/wallet/balances?pkscript=0".into(),
            "pkscript must be hex of at most 20000 digits",
        ),
        (
            "/v1/runes/wallet/balances?address=a%00".into(),
            "address must be at most 128 bytes without NUL",
        ),
        (
            "/v1/runes/wallet/balances?address=a&after=1.5".into(),
            after,
        ),
        (
            "/v1/runes/rune/events?rune=A&before_id=0".into(),
            "before_id must be an integer from 1 to 999999999999999999",
        ),
        (
            "/v1/runes/rune/events?rune=A&limit=x".into(),
            "limit must be an integer from 1 to 500",
        ),
        (
            "/v1/runes/rune/events?rune=A&type=%00".into(),
            "type must be at most 256 bytes without NUL",
        ),
        ("/v1/runes/rune/events".into(), "rune is required"),
        (
            "/v1/runes/wallet/events".into(),
            "address or pkscript is required",
        ),
        (
            "/v1/runes/wallet/events?pkscript=zz".into(),
            "pkscript must be hex of at most 20000 digits",
        ),
    ] {
        expect(&app, &uri, 400, JSON, &bad(reason)).await;
    }
}

#[tokio::test]
async fn valid_requests_answer_500_when_the_query_fails() {
    let app = height(RUNES, 7.into());
    for uri in [
        "/v1/runes/rune?rune=1%3A0",
        "/v1/runes/runes",
        "/v1/runes/runes?search=A.B&sort=supply&after=0.5%3A1",
        "/v1/runes/rune/holders?rune=UNCOMMON.GOODS",
        "/v1/runes/wallet/balances?address=tb1q",
        "/v1/runes/rune/events?rune=1%3A0&type=mint",
        "/v1/runes/wallet/events?pkscript=0014",
    ] {
        expect(&app, uri, 500, JSON, INTERNAL_ERROR).await;
    }
}

/// Four runes: `1:0` (UNCOMMON•GOODS, the mainnet seed's terms), `10:1`
/// (FIR•STRUNE, divisibility 2, half minted, seven holders), `20:1`
/// (SECONDRUNE, no terms) and `30:2` (UNCOMMONGIFTS). Block 11's hash row is
/// gone, as pruning leaves it.
const FIXTURE: &str = r#"
INSERT INTO runes_block_hashes (block_height, block_hash, block_time)
    SELECT h, 'hash' || h, to_timestamp(1700000000 + h) FROM generate_series(1, 40) h WHERE h <> 11;
INSERT INTO runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching, terms_amount,
    terms_cap, terms_height_l, terms_height_h, terms_offset_l, terms_offset_h, mints, "number",
    premine, rune_name, spacers, symbol, "timestamp", turbo, genesis_height,
    last_updated_block_height) VALUES
('1:0', 1, 0, 0, repeat('0', 64), 1, 340282366920938463463374607431768211455, 840000, 1050000,
    NULL, NULL, 3, 0, 0, 'UNCOMMONGOODS', 128, '⧉', to_timestamp(0), true, 1, 1),
('10:1', 10, 7, 2, repeat('a', 64), 50, 4, NULL, 30, 0, NULL, 2, 1, 1000, 'FIRSTRUNE', 4,
    '24000000', to_timestamp(1700000010), false, 10, 12),
('20:1', 20, 0, 0, repeat('b', 64), NULL, NULL, NULL, NULL, NULL, NULL, 0, 2, 5, 'SECONDRUNE', 0,
    NULL, to_timestamp(1700000020), false, 20, 20),
('30:2', 30, 0, 8, repeat('c', 64), 10, 10, NULL, NULL, NULL, NULL, 1, 3, 0, 'UNCOMMONGIFTS', 0,
    'zz', to_timestamp(1700000030), false, 30, 31);
INSERT INTO runes_outpoint_to_balances (outpoint, pkscript, wallet_addr, rune_ids, balances,
    block_height, spent, spent_block_height) VALUES
('o1:0', '001401', 'addr1', '{10:1,30:2}', '{20,10}', 12, false, NULL),
('o1:1', '001401', 'addr1', '{10:1,1:0}', '{10,2}', 12, false, NULL),
('o2:0', '001402', 'addr2', '{10:1}', '{10}', 12, false, NULL),
('o2:1', '001402', 'addr2', '{10:1}', '{1000}', 11, true, 12),
('o2:2', '001402', 'addr2', '{20:1}', '{5}', 20, false, NULL),
('o3:0', '001403', 'addr3', '{10:1}', '{20}', 12, false, NULL),
('o4:0', '001404', 'addr4', '{10:1}', '{30}', 12, false, NULL),
('o5:0', '001405', 'addr5', '{10:1}', '{10}', 12, false, NULL),
('o6:0', '001406', 'addr6', '{10:1}', '{20}', 12, false, NULL),
('o7:0', '001407', 'addr7', '{10:1}', '{30}', 12, false, NULL),
('o8:0', 'ffff', NULL, '{1:0}', '{1}', 2, false, NULL);
INSERT INTO runes_events (id, event_type, txid, outpoint, pkscript, wallet_addr, rune_id, amount,
    block_height) VALUES
(0, 1, 't10', NULL, NULL, NULL, '10:1', 1000, 10),
(1, 3, 't11', 'o2:1', '001402', 'addr2', '10:1', 1000, 11),
(2, 2, 't11', NULL, NULL, NULL, '10:1', 50, 11),
(3, 0, 't12', 'o2:1', '001402', 'addr2', '10:1', 1000, 12),
(4, 3, 't12', 'o1:0', '001401', 'addr1', '10:1', 20, 12),
(5, 3, 't12', 'o1:0', '001401', 'addr1', '30:2', 10, 12),
(6, 3, 't12', 'o1:1', '001401', 'addr1', '10:1', 10, 12),
(7, 3, 't12', 'o2:0', '001402', 'addr2', '10:1', 10, 12),
(8, 4, 't12', NULL, NULL, NULL, '10:1', 7, 12);
DROP TABLE runes_current_balances;
DROP TABLE runes_holders;
"#;

/// Runs `body` on the seeded database, after the runes module's start-up
/// fill rebuilt the holder tables.
async fn with_runes<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Router, PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let schema = include_str!("../../opi-runes/src/db_init.sql");
    with_seeded(
        RUNES,
        tag,
        None,
        &[schema, FIXTURE],
        |app, pool| async move {
            assert!(opi_runes::holders::create_if_missing(&pool).await.unwrap());
            body(app, pool).await;
        },
    )
    .await;
}

/// Before the runes module built its tables and indexes, every rune route
/// answers 503 (after its parameter checks); once they exist, the routes
/// answer, and the answer stays cached.
#[tokio::test]
async fn the_routes_answer_503_until_the_tables_are_built() {
    let schema = include_str!("../../opi-runes/src/db_init.sql");
    with_seeded(
        RUNES,
        "unbuilt",
        None,
        &[schema, FIXTURE],
        |app, pool| async move {
            let unbuilt = bad("the runes API tables are not built yet; start opi-indexer run once");
            let uris = [
                "/v1/runes/rune?rune=10%3A1",
                "/v1/runes/runes",
                "/v1/runes/rune/holders?rune=FIRSTRUNE",
                "/v1/runes/wallet/balances?address=addr1",
                "/v1/runes/rune/events?rune=10%3A1",
                "/v1/runes/wallet/events?pkscript=001401",
            ];
            for uri in uris {
                expect(&app, uri, 503, JSON, &unbuilt).await;
            }
            // parameters are still checked first
            expect(&app, "/v1/runes/rune", 400, JSON, &bad("rune is required")).await;
            // the parity routes do not need the tables
            assert_eq!(
                get(&app, "/v1/runes/holders?rune_name=FIRSTRUNE")
                    .await
                    .status,
                200
            );
            // tables without the indexes are not built yet
            sqlx::raw_sql(sqlx::AssertSqlSafe(opi_runes::holders::TABLES_SQL))
                .execute(&pool)
                .await
                .unwrap();
            expect(&app, uris[0], 503, JSON, &unbuilt).await;
            sqlx::raw_sql("DROP TABLE runes_current_balances; DROP TABLE runes_holders;")
                .execute(&pool)
                .await
                .unwrap();
            assert!(opi_runes::holders::create_if_missing(&pool).await.unwrap());
            for uri in uris {
                assert_eq!(get(&app, uri).await.status, 200, "{uri}");
            }
        },
    )
    .await;
}

/// The answer to `uri`, which must be `status`, as JSON.
async fn answer(app: &Router, uri: &str, status: u16) -> Value {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, status, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(JSON), "{uri}");
    serde_json::from_str(&answer.body).unwrap()
}

async fn result(app: &Router, uri: &str) -> Value {
    let body = answer(app, uri, 200).await;
    assert_eq!(body["error"], Value::Null, "{uri}");
    body["result"].clone()
}

/// Every page of a list from `first` on, following `cursor` (`after` or
/// `before_id`), as the rows of `field`.
async fn pages(app: &Router, first: &str, field: &str, cursor: &str) -> Vec<Vec<Value>> {
    let next = format!("next_{cursor}");
    let mut uri = first.to_string();
    let mut pages = Vec::new();
    loop {
        let page = result(app, &uri).await;
        pages.push(page[field].as_array().unwrap().clone());
        if !page["has_more"].as_bool().unwrap() {
            assert_eq!(page[&next], Value::Null);
            return pages;
        }
        let value = match &page[&next] {
            Value::String(text) => encode(text),
            other => other.to_string(),
        };
        uri = format!("{first}&{cursor}={value}");
    }
}

fn ids(rows: &[Vec<Value>], field: &str) -> Vec<Vec<String>> {
    rows.iter()
        .map(|page| {
            page.iter()
                .map(|row| match &row[field] {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .collect()
}

fn strings(pages: &[&[&str]]) -> Vec<Vec<String>> {
    pages
        .iter()
        .map(|page| page.iter().map(|s| s.to_string()).collect())
        .collect()
}

#[tokio::test]
async fn a_rune_answers_by_id_or_name() {
    with_runes("rune", |app, _| async move {
        let rune = result(&app, "/v1/runes/rune?rune=10%3A1").await;
        assert_eq!(
            rune,
            json!({
                "rune_id": "10:1",
                "rune_name": "FIRSTRUNE",
                "spaced_rune_name": "FIR•STRUNE",
                "number": "1",
                "divisibility": 2,
                "symbol": "$",
                "spacers": 4,
                "etching": "a".repeat(64),
                "etching_block_height": 10,
                "timestamp": 1700000010,
                "turbo": false,
                "premine": "1000",
                "mints": "2",
                "burned": "7",
                "supply": "1100",
                "max_supply": "1200",
                "terms": {
                    "amount": "50",
                    "cap": "4",
                    "height_start": null,
                    "height_end": 30,
                    "offset_start": 0,
                    "offset_end": null,
                },
                "mint_progress": 0.5,
                "holders": 7,
                "block_height": 40,
            })
        );
        // any spacing of the letters finds the rune, which answers its own
        // spaced name
        for name in [
            "FIRSTRUNE",
            "firstrune",
            "fir.strune",
            "FIR%E2%80%A2STRUNE",
            "FI.RSTRUNE",
            "f.i.r.s.t.r.u.n.e",
            "010%3A01",
        ] {
            assert_eq!(
                result(&app, &format!("/v1/runes/rune?rune={name}")).await,
                rune,
                "{name}"
            );
        }
        let holders = result(&app, "/v1/runes/rune/holders?rune=FIRSTRU.NE&limit=1").await;
        assert_eq!(holders["spaced_rune_name"], "FIR•STRUNE");
        let events = result(&app, "/v1/runes/rune/events?rune=F.IRSTRUNE&limit=1").await;
        assert_eq!(events["rune_id"], "10:1");
        for missing in ["NOSUCHRUNE", "FIRSTRUNEX", "99%3A1"] {
            expect(
                &app,
                &format!("/v1/runes/rune?rune={missing}"),
                404,
                JSON,
                NOT_FOUND,
            )
            .await;
        }

        let seed = result(&app, "/v1/runes/rune?rune=uncommon.goods").await;
        assert_eq!(seed["spaced_rune_name"], "UNCOMMON•GOODS");
        assert_eq!(seed["symbol"], "⧉");
        assert_eq!(seed["supply"], "3");
        assert_eq!(
            seed["max_supply"],
            "340282366920938463463374607431768211455"
        );
        assert_eq!(seed["holders"], 2);
        let plain = result(&app, "/v1/runes/rune?rune=SECONDRUNE").await;
        assert_eq!(plain["terms"], Value::Null);
        assert_eq!(plain["mint_progress"], Value::Null);
        assert_eq!(plain["symbol"], Value::Null);
        assert_eq!(plain["max_supply"], "5");
        // a stored symbol that is not one character answers null
        let gifts = result(&app, "/v1/runes/rune?rune=30%3A2").await;
        assert_eq!(gifts["symbol"], Value::Null);
        assert_eq!(gifts["divisibility"], 8);
    })
    .await;
}

#[tokio::test]
async fn the_runes_list_sorts_searches_and_pages() {
    with_runes("runes", |app, _| async move {
        for (query, order) in [
            ("", vec!["10:1", "1:0", "30:2", "20:1"]),
            (
                "sort=holders&order=asc",
                vec!["20:1", "30:2", "1:0", "10:1"],
            ),
            ("sort=supply", vec!["10:1", "30:2", "20:1", "1:0"]),
            ("sort=mint_progress", vec!["10:1", "30:2", "1:0", "20:1"]),
            (
                "sort=mint_progress&order=asc",
                vec!["20:1", "1:0", "30:2", "10:1"],
            ),
            (
                "sort=etching_height&order=asc",
                vec!["1:0", "10:1", "20:1", "30:2"],
            ),
            ("sort=etching_height", vec!["30:2", "20:1", "10:1", "1:0"]),
            ("search=uncommong", vec!["1:0", "30:2"]),
            ("search=UNCOMMON%E2%80%A2G", vec!["1:0"]),
            ("search=uncommon.", vec!["1:0"]),
            ("search=FIR.S", vec!["10:1"]),
            ("search=FI.R", vec![]),
            ("search=Z", vec![]),
        ] {
            let all = result(&app, &format!("/v1/runes/runes?{query}")).await;
            let listed: Vec<&str> = all["runes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|rune| rune["rune_id"].as_str().unwrap())
                .collect();
            assert_eq!(listed, order, "{query}");
            // every page of 1 and of 3 lists the same runes, without dupes
            // or gaps
            for limit in [1, 3] {
                let paged = pages(
                    &app,
                    &format!("/v1/runes/runes?{query}&limit={limit}"),
                    "runes",
                    "after",
                )
                .await;
                let flat: Vec<String> = ids(&paged, "rune_id").concat();
                assert_eq!(flat, order, "{query} limit {limit}");
                assert!(paged.iter().all(|page| page.len() <= limit));
            }
        }
        let page = result(&app, "/v1/runes/runes?limit=2&sort=supply&order=asc").await;
        assert_eq!(page["sort"], "supply");
        assert_eq!(page["order"], "asc");
        assert_eq!(page["limit"], 2);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["next_after"], "5:2");
        assert_eq!(page["block_height"], 40);
        assert_eq!(page["runes"][0]["rune_id"], "1:0");
    })
    .await;
}

#[tokio::test]
async fn a_runes_holders_page_by_balance() {
    with_runes("holders", |app, pool| async move {
        let paged = pages(
            &app,
            "/v1/runes/rune/holders?rune=FIRSTRUNE&limit=3",
            "holders",
            "after",
        )
        .await;
        assert_eq!(
            ids(&paged, "pkscript"),
            strings(&[
                &["001407", "001404", "001401"],
                &["001406", "001403", "001405"],
                &["001402"],
            ])
        );
        assert_eq!(
            ids(&paged, "balance").concat(),
            ["30", "30", "30", "20", "20", "10", "10"]
        );
        assert_eq!(
            paged[0][2],
            json!({"address": "addr1", "pkscript": "001401", "balance": "30"})
        );
        let page = result(&app, "/v1/runes/rune/holders?rune=FIR.STRUNE&limit=3").await;
        assert_eq!(page["rune_id"], "10:1");
        assert_eq!(page["spaced_rune_name"], "FIR•STRUNE");
        assert_eq!(page["divisibility"], 2);
        assert_eq!(page["next_after"], "30:001401");
        // a script without an address
        let seed = result(&app, "/v1/runes/rune/holders?rune=1%3A0").await;
        assert_eq!(
            seed["holders"],
            json!([
                {"address": "addr1", "pkscript": "001401", "balance": "2"},
                {"address": null, "pkscript": "ffff", "balance": "1"},
            ])
        );
        expect(
            &app,
            "/v1/runes/rune/holders?rune=NOSUCHRUNE",
            404,
            JSON,
            NOT_FOUND,
        )
        .await;

        // The holder counts are a recount of the unspent outpoints.
        let recount: Vec<(String, i64)> = sqlx::query_as(
            "select u.rune_id, count(distinct o.pkscript)
             from runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) u(rune_id, balance)
             where not o.spent and u.balance > 0 group by 1 order by 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for (rune_id, holders) in recount {
            let rune = result(&app, &format!("/v1/runes/rune?rune={}", encode(&rune_id))).await;
            assert_eq!(rune["holders"], holders, "{rune_id}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_wallets_balances_list_its_runes() {
    with_runes("wallet", |app, _| async move {
        let by_address = result(&app, "/v1/runes/wallet/balances?address=addr1").await;
        assert_eq!(by_address["address"], "addr1");
        assert_eq!(by_address["pkscript"], Value::Null);
        assert_eq!(
            by_address["balances"],
            json!([
                {"rune_id": "1:0", "rune_name": "UNCOMMONGOODS", "spaced_rune_name": "UNCOMMON•GOODS",
                 "number": "0", "divisibility": 0, "symbol": "⧉", "balance": "2"},
                {"rune_id": "10:1", "rune_name": "FIRSTRUNE", "spaced_rune_name": "FIR•STRUNE",
                 "number": "1", "divisibility": 2, "symbol": "$", "balance": "30"},
                {"rune_id": "30:2", "rune_name": "UNCOMMONGIFTS", "spaced_rune_name": "UNCOMMONGIFTS",
                 "number": "3", "divisibility": 8, "symbol": null, "balance": "10"},
            ])
        );
        let by_pkscript = result(&app, "/v1/runes/wallet/balances?pkscript=001401").await;
        assert_eq!(by_pkscript["pkscript"], "001401");
        assert_eq!(by_pkscript["address"], Value::Null);
        assert_eq!(by_pkscript["balances"], by_address["balances"]);
        let paged = pages(&app, "/v1/runes/wallet/balances?address=addr1&limit=2", "balances", "after").await;
        assert_eq!(ids(&paged, "rune_id"), strings(&[&["1:0", "10:1"], &["30:2"]]));
        // the spent outpoint of 001402 does not count
        let spent = result(&app, "/v1/runes/wallet/balances?pkscript=001402").await;
        assert_eq!(ids(&[spent["balances"].as_array().unwrap().clone()], "balance"), strings(&[&["10", "5"]]));
        let unknown = result(&app, "/v1/runes/wallet/balances?address=nobody").await;
        assert_eq!(unknown["balances"], json!([]));
        assert_eq!(unknown["has_more"], false);
    })
    .await;
}

#[tokio::test]
async fn the_event_feeds_page_by_id() {
    with_runes("events", |app, _| async move {
        let paged = pages(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&limit=3",
            "events",
            "before_id",
        )
        .await;
        assert_eq!(
            ids(&paged, "id"),
            strings(&[&["8", "7", "6"], &["4", "3", "2"], &["1", "0"]])
        );
        let page = result(&app, "/v1/runes/rune/events?rune=firstrune&limit=3").await;
        assert_eq!(page["next_before_id"], 6);
        assert_eq!(page["divisibility"], 2);
        assert_eq!(page["event_type"], Value::Null);
        assert_eq!(
            page["events"][2],
            json!({
                "id": 6, "block_height": 12, "block_time": 1700000012, "event_type": "output",
                "txid": "t12", "outpoint": "o1:1", "pkscript": "001401", "address": "addr1",
                "rune_id": "10:1", "rune_name": "FIRSTRUNE", "spaced_rune_name": "FIR•STRUNE",
                "divisibility": 2, "amount": "10",
            })
        );
        // block 11's time is unknown
        let older = result(&app, "/v1/runes/rune/events?rune=10%3A1&before_id=3").await;
        assert_eq!(older["events"][0]["id"], 2);
        assert_eq!(older["events"][0]["block_time"], Value::Null);
        assert_eq!(older["events"][0]["event_type"], "mint");
        assert_eq!(older["events"][0]["outpoint"], Value::Null);

        let outputs = pages(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&type=output&limit=2",
            "events",
            "before_id",
        )
        .await;
        assert_eq!(ids(&outputs, "id"), strings(&[&["7", "6"], &["4", "1"]]));
        let types = "type must be one of input, new-allocation, mint, output, burn";
        expect(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&type=transfer",
            400,
            JSON,
            &bad(types),
        )
        .await;
        expect(
            &app,
            "/v1/runes/rune/events?rune=NOSUCHRUNE",
            404,
            JSON,
            NOT_FOUND,
        )
        .await;

        let wallet = pages(
            &app,
            "/v1/runes/wallet/events?address=addr1&limit=2",
            "events",
            "before_id",
        )
        .await;
        assert_eq!(ids(&wallet, "id"), strings(&[&["6", "5"], &["4"]]));
        assert_eq!(wallet[0][1]["rune_id"], "30:2");
        assert_eq!(wallet[0][1]["divisibility"], 8);
        let by_pkscript = result(&app, "/v1/runes/wallet/events?pkscript=001402&type=input").await;
        assert_eq!(by_pkscript["pkscript"], "001402");
        assert_eq!(by_pkscript["event_type"], "input");
        assert_eq!(
            ids(&[by_pkscript["events"].as_array().unwrap().clone()], "id"),
            strings(&[&["3"]])
        );
        let none = result(&app, "/v1/runes/wallet/events?address=nobody").await;
        assert_eq!(none["events"], json!([]));
        expect(
            &app,
            "/v1/runes/wallet/events?address=a&type=x",
            400,
            JSON,
            &bad(types),
        )
        .await;
    })
    .await;
}

/// The live database: every route answers its shape, the holders and
/// balances agree with the parity routes, the holder counts with a recount,
/// and the paged lists hold no dupes or gaps.
#[tokio::test]
async fn live_routes_agree_with_the_parity_routes() {
    let Some((app, pool)) = live(RUNES, "OPI_API_TEST_RUNES_URL") else {
        return;
    };
    // the runes with the most holders, by a recount
    let top: Vec<(String, String, i64)> = sqlx::query_as(
        "select u.rune_id, e.rune_name, count(distinct o.pkscript)
         from runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) u(rune_id, balance)
         join runes_id_to_entry e on e.rune_id = u.rune_id
         where not o.spent and u.balance > 0 group by 1, 2 order by 3 desc, 1 limit 5",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!top.is_empty());
    for (rune_id, rune_name, holders) in &top {
        let rune = result(&app, &format!("/v1/runes/rune?rune={}", encode(rune_id))).await;
        assert_eq!(rune["holders"], *holders, "{rune_id}");
        let by_name = result(
            &app,
            &format!("/v1/runes/rune?rune={}", encode(&rune_name.to_lowercase())),
        )
        .await;
        assert_eq!(by_name, rune);
        let spaced = rune["spaced_rune_name"].as_str().unwrap();
        assert_eq!(
            result(&app, &format!("/v1/runes/rune?rune={}", encode(spaced))).await,
            rune
        );

        // the holders route against the parity route
        let paged = pages(
            &app,
            &format!("/v1/runes/rune/holders?rune={}&limit=37", encode(rune_id)),
            "holders",
            "after",
        )
        .await;
        let ours: Vec<(String, String)> = paged
            .concat()
            .iter()
            .map(|h| {
                (
                    h["pkscript"].as_str().unwrap().to_string(),
                    h["balance"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(ours.len() as i64, *holders, "{rune_id}");
        let unique: BTreeSet<&String> = ours.iter().map(|(pk, _)| pk).collect();
        assert_eq!(unique.len(), ours.len(), "{rune_id}: a holder twice");
        let parity = answer(
            &app,
            &format!("/v1/runes/holders?rune_name=&rune_id={}", encode(rune_id)),
            200,
        )
        .await;
        let mut theirs: Vec<(String, String)> = parity["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["pkscript"].as_str().unwrap().to_string(),
                    h["total_balance"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let mut sorted = ours.clone();
        sorted.sort();
        theirs.sort();
        assert_eq!(sorted, theirs, "{rune_id}");

        let events = pages(
            &app,
            &format!("/v1/runes/rune/events?rune={}&limit=200", encode(rune_id)),
            "events",
            "before_id",
        )
        .await;
        let event_ids: Vec<i64> = events
            .concat()
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect();
        assert!(event_ids.windows(2).all(|w| w[0] > w[1]), "{rune_id}");
        let count: i64 = sqlx::query_scalar("select count(*) from runes_events where rune_id = $1")
            .bind(rune_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(event_ids.len() as i64, count, "{rune_id}");
    }

    // a busy wallet's balances against the parity route
    let (wallet, pkscript): (String, String) = sqlx::query_as(
        "select wallet_addr, pkscript from runes_outpoint_to_balances
         where not spent and wallet_addr is not null
         group by 1, 2 order by count(*) desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let balances = pages(
        &app,
        &format!(
            "/v1/runes/wallet/balances?address={}&limit=7",
            encode(&wallet)
        ),
        "balances",
        "after",
    )
    .await;
    let mut ours: Vec<(String, String)> = balances
        .concat()
        .iter()
        .map(|b| {
            (
                b["rune_id"].as_str().unwrap().to_string(),
                b["balance"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let parity = answer(
        &app,
        &format!(
            "/v1/runes/get_current_balance_of_wallet?address={}",
            encode(&wallet)
        ),
        200,
    )
    .await;
    let mut theirs: Vec<(String, String)> = parity["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["total_balance"] != "0")
        .map(|b| {
            (
                b["rune_id"].as_str().unwrap().to_string(),
                b["total_balance"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    ours.sort();
    theirs.sort();
    assert_eq!(ours, theirs);
    let by_pkscript = pages(
        &app,
        &format!("/v1/runes/wallet/balances?pkscript={pkscript}&limit=7"),
        "balances",
        "after",
    )
    .await;
    assert_eq!(by_pkscript, balances);
    let events = pages(
        &app,
        &format!(
            "/v1/runes/wallet/events?address={}&limit=100",
            encode(&wallet)
        ),
        "events",
        "before_id",
    )
    .await;
    let count: i64 = sqlx::query_scalar("select count(*) from runes_events where wallet_addr = $1")
        .bind(&wallet)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(events.concat().len() as i64, count);

    // every list order pages without dupes or gaps
    let total: i64 = sqlx::query_scalar("select count(*) from runes_id_to_entry")
        .fetch_one(&pool)
        .await
        .unwrap();
    for sort in ["holders", "supply", "mint_progress", "etching_height"] {
        for order in ["desc", "asc"] {
            let paged = pages(
                &app,
                &format!("/v1/runes/runes?sort={sort}&order={order}&limit=500"),
                "runes",
                "after",
            )
            .await;
            let all: Vec<String> = ids(&paged, "rune_id").concat();
            let unique: BTreeSet<&String> = all.iter().collect();
            assert_eq!(unique.len(), all.len(), "{sort} {order}");
            assert_eq!(all.len() as i64, total, "{sort} {order}");
        }
    }
    let search = result(
        &app,
        &format!("/v1/runes/runes?search={}", encode(&top[0].1[..2])),
    )
    .await;
    assert!(
        search["runes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["rune_name"].as_str().unwrap().starts_with(&top[0].1[..2]))
    );
}
