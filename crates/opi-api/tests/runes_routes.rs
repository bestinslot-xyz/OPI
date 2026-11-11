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
    common::{
        INTERNAL_ERROR, JSON, encode, expect, get, height, live, live_runes_wallet, with_seeded,
    },
    opi_api::config::Module,
    serde_json::{Value, json},
    sqlx::PgPool,
    std::collections::BTreeSet,
};

const RUNES: Module = Module::Runes;
const NOT_FOUND: &str = r#"{"error":"rune not found","result":null}"#;

/// A 1-of-3 bare multisig of uncompressed keys: 402 hex digits.
fn multisig() -> String {
    let script = format!("51{}53ae", format!("4104{}", "ab".repeat(64)).repeat(3));
    assert_eq!(script.len(), 402);
    script
}

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
        (
            format!(
                "/v1/runes/rune/holders?rune=A&after=1%3A{}",
                "a".repeat(20080)
            ),
            "after must be at most 20081 bytes without NUL",
        ),
        (
            format!("/v1/runes/runes?after=1%3A{}", "1".repeat(255)),
            "after must be at most 256 bytes without NUL",
        ),
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

/// The txid of 32 bytes `n`, as hex.
fn tx(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// The outpoint `vout` of [`tx`] `n`.
fn op(n: u8, vout: u32) -> String {
    format!("{}:{vout}", tx(n))
}

/// The P2WPKH pkScript of holder `n`, 20 bytes `n`, as hex.
fn pk(n: u8) -> String {
    format!("0014{}", format!("{n:02x}").repeat(20))
}

/// The regtest address of [`pk`] `n`.
fn addr(n: u8) -> String {
    opi_runes::wallet_addr(&hex::decode(pk(n)).unwrap(), "regtest").unwrap()
}

/// Four runes: `1:0` (UNCOMMON•GOODS, the mainnet seed's terms), `10:1`
/// (FIR•STRUNE, divisibility 2, half minted, seven holders), `20:1`
/// (SECONDRUNE, no terms) and `30:2` (UNCOMMONGIFTS), on regtest. Holder
/// `n` is [`pk`] `n` and its outpoints are on [`tx`] `n`; the events are
/// those of txids `0x10` to `0x12`. Block 11's hash row is gone, as pruning
/// leaves it.
const FIXTURE: &str = r#"
INSERT INTO runes_network_type (network_type) VALUES ('regtest');
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
INSERT INTO runes_outpoint_to_balances (txid, vout, pkscript, rune_ids, balances, block_height,
    spent_block_height)
SELECT decode(repeat(t, 32), 'hex'), vout, decode(pk, 'hex'), ids::text[], bal::numeric[], h, s
FROM (VALUES
('01', 0, '0014' || repeat('01', 20), '{10:1,30:2}', '{20,10}', 12, NULL::int4),
('01', 1, '0014' || repeat('01', 20), '{10:1,1:0}', '{10,2}', 12, NULL),
('02', 0, '0014' || repeat('02', 20), '{10:1}', '{10}', 12, NULL),
('02', 1, '0014' || repeat('02', 20), '{10:1}', '{1000}', 11, 12),
('02', 2, '0014' || repeat('02', 20), '{20:1}', '{5}', 20, NULL),
('03', 0, '0014' || repeat('03', 20), '{10:1}', '{20}', 12, NULL),
('04', 0, '0014' || repeat('04', 20), '{10:1}', '{30}', 12, NULL),
('05', 0, '0014' || repeat('05', 20), '{10:1}', '{10}', 12, NULL),
('06', 0, '0014' || repeat('06', 20), '{10:1}', '{20}', 12, NULL),
('07', 0, '0014' || repeat('07', 20), '{10:1}', '{30}', 12, NULL),
('08', 0, 'ffff', '{1:0}', '{1}', 2, NULL),
('09', 0, '51' || repeat('4104' || repeat('ab', 64), 3) || '53ae', '{1:0}', '{2}', 2, NULL)
) v(t, vout, pk, ids, bal, h, s);
INSERT INTO runes_events (id, event_type, txid, outpoint_txid, outpoint_vout, pkscript, rune_id,
    amount, block_height)
SELECT id, kind, decode(repeat(t, 32), 'hex'), decode(repeat(o, 32), 'hex'), vout,
    decode('0014' || repeat(o, 20), 'hex'), rune, amount, h
FROM (VALUES
(0, 1, '10', NULL, NULL::int4, '10:1', 1000, 10),
(1, 3, '11', '02', 1, '10:1', 1000, 11),
(2, 2, '11', NULL, NULL, '10:1', 50, 11),
(3, 0, '12', '02', 1, '10:1', 1000, 12),
(4, 3, '12', '01', 0, '10:1', 20, 12),
(5, 3, '12', '01', 0, '30:2', 10, 12),
(6, 3, '12', '01', 1, '10:1', 10, 12),
(7, 3, '12', '02', 0, '10:1', 10, 12),
(8, 4, '12', NULL, NULL, '10:1', 7, 12)
) v(id, kind, t, o, vout, rune, amount, h);
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
                "/v1/runes/rune?rune=10%3A1".to_string(),
                "/v1/runes/runes".into(),
                "/v1/runes/etchings".into(),
                "/v1/runes/rune/holders?rune=FIRSTRUNE".into(),
                format!("/v1/runes/wallet/balances?address={}", addr(1)),
                "/v1/runes/rune/events?rune=10%3A1".into(),
                format!("/v1/runes/wallet/events?pkscript={}", pk(1)),
                // the parity holders route reads runes_current_balances
                "/v1/runes/holders?rune_name=FIRSTRUNE".into(),
            ];
            for uri in &uris {
                expect(&app, uri, 503, JSON, &unbuilt).await;
            }
            // parameters are still checked first
            expect(&app, "/v1/runes/rune", 400, JSON, &bad("rune is required")).await;
            // the other parity routes do not need the tables
            let parity = format!("/v1/runes/get_current_balance_of_wallet?pkscript={}", pk(1));
            assert_eq!(get(&app, &parity).await.status, 200);
            // tables without the indexes are not built yet
            sqlx::raw_sql(sqlx::AssertSqlSafe(opi_runes::holders::TABLES_SQL))
                .execute(&pool)
                .await
                .unwrap();
            expect(&app, &uris[0], 503, JSON, &unbuilt).await;
            sqlx::raw_sql("DROP TABLE runes_current_balances; DROP TABLE runes_holders;")
                .execute(&pool)
                .await
                .unwrap();
            assert!(opi_runes::holders::create_if_missing(&pool).await.unwrap());
            for uri in &uris {
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
        assert_eq!(seed["holders"], 3);
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
        let order = [[7, 4, 1], [6, 3, 5]];
        assert_eq!(
            ids(&paged, "pkscript"),
            vec![
                order[0].map(pk).to_vec(),
                order[1].map(pk).to_vec(),
                vec![pk(2)],
            ]
        );
        assert_eq!(
            ids(&paged, "balance").concat(),
            ["30", "30", "30", "20", "20", "10", "10"]
        );
        assert_eq!(
            paged[0][2],
            json!({"address": addr(1), "pkscript": pk(1), "balance": "30"})
        );
        let page = result(&app, "/v1/runes/rune/holders?rune=FIR.STRUNE&limit=3").await;
        assert_eq!(page["rune_id"], "10:1");
        assert_eq!(page["spaced_rune_name"], "FIR•STRUNE");
        assert_eq!(page["divisibility"], 2);
        assert_eq!(page["next_after"], format!("30:{}", pk(1)));
        // a cursor's pkScript may be upper case
        let upper = result(
            &app,
            &format!(
                "/v1/runes/rune/holders?rune=FIRSTRUNE&limit=3&after=30%3A{}",
                pk(1).to_uppercase()
            ),
        )
        .await;
        assert_eq!(upper["holders"], json!(paged[1]));

        // the parity route lists the same holders, largest first, from the
        // same table, with the address and the rune name
        let parity = answer(&app, "/v1/runes/holders?rune_name=firstrune", 200).await;
        let listed: Vec<(String, String)> = parity["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                assert_eq!(h["rune_id"], "10:1");
                assert_eq!(h["rune_name"], "FIRSTRUNE");
                (
                    h["pkscript"].as_str().unwrap().to_string(),
                    h["total_balance"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let ours: Vec<(String, String)> = paged
            .concat()
            .iter()
            .map(|h| {
                (
                    h["pkscript"].as_str().unwrap().into(),
                    h["balance"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(listed, ours);
        assert_eq!(parity["result"][2]["wallet_addr"], addr(1));
        assert_eq!(parity["db_block_height"], 40);
        // scripts without an address, one a 402-digit bare multisig, which
        // a page can end on: its cursor is longer than the other lists'
        let multisig = multisig();
        let seed = result(&app, "/v1/runes/rune/holders?rune=1%3A0").await;
        assert_eq!(
            seed["holders"],
            json!([
                {"address": null, "pkscript": multisig, "balance": "2"},
                {"address": addr(1), "pkscript": pk(1), "balance": "2"},
                {"address": null, "pkscript": "ffff", "balance": "1"},
            ])
        );
        let paged = pages(
            &app,
            "/v1/runes/rune/holders?rune=1%3A0&limit=1",
            "holders",
            "after",
        )
        .await;
        assert_eq!(
            ids(&paged, "pkscript"),
            vec![vec![multisig.clone()], vec![pk(1)], vec!["ffff".into()]]
        );
        let first = result(&app, "/v1/runes/rune/holders?rune=1%3A0&limit=1").await;
        assert_eq!(first["next_after"], format!("2:{multisig}"));
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
             where o.spent_block_height is null and u.balance > 0 group by 1 order by 1",
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
        let by_address = result(
            &app,
            &format!("/v1/runes/wallet/balances?address={}", addr(1)),
        )
        .await;
        assert_eq!(by_address["address"], addr(1));
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
        let by_pkscript = result(
            &app,
            &format!("/v1/runes/wallet/balances?pkscript={}", pk(1).to_uppercase()),
        )
        .await;
        assert_eq!(by_pkscript["pkscript"], pk(1));
        assert_eq!(by_pkscript["address"], Value::Null);
        assert_eq!(by_pkscript["balances"], by_address["balances"]);
        let paged = pages(
            &app,
            &format!("/v1/runes/wallet/balances?address={}&limit=2", addr(1)),
            "balances",
            "after",
        )
        .await;
        assert_eq!(ids(&paged, "rune_id"), strings(&[&["1:0", "10:1"], &["30:2"]]));
        // the spent outpoint of holder 2 does not count
        let spent = result(&app, &format!("/v1/runes/wallet/balances?pkscript={}", pk(2))).await;
        assert_eq!(ids(&[spent["balances"].as_array().unwrap().clone()], "balance"), strings(&[&["10", "5"]]));
        // an address that encodes no pkScript here: not one, another
        // network's, a script type without an address form
        let mainnet = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        for unknown in ["nobody", mainnet, "bcrt1zw508d6qejxtdg4y5r3zarvaryvg6kdaj"] {
            let page = result(&app, &format!("/v1/runes/wallet/balances?address={unknown}")).await;
            assert_eq!(page["address"], unknown);
            assert_eq!(page["balances"], json!([]));
            assert_eq!(page["has_more"], false);
        }
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
                "txid": tx(0x12), "outpoint": op(1, 1), "pkscript": pk(1), "address": addr(1),
                "rune_id": "10:1", "rune_name": "FIRSTRUNE", "spaced_rune_name": "FIR•STRUNE",
                "divisibility": 2, "amount": "10",
            })
        );
        assert_eq!(page["category"], Value::Null);
        // block 11's time is unknown
        let older = result(&app, "/v1/runes/rune/events?rune=10%3A1&before_id=3").await;
        assert_eq!(older["events"][0]["id"], 2);
        assert_eq!(older["events"][0]["block_time"], Value::Null);
        assert_eq!(older["events"][0]["event_type"], "mint");
        assert_eq!(older["events"][0]["outpoint"], Value::Null);
        assert_eq!(older["events"][0]["pkscript"], Value::Null);
        assert_eq!(older["events"][0]["address"], Value::Null);
        assert_eq!(older["events"][0]["txid"], tx(0x11));

        // categories: etch is new-allocation, transfer is input and output
        for (category, expected) in [
            ("etch", vec!["0"]),
            ("mint", vec!["2"]),
            ("burn", vec!["8"]),
            ("transfer", vec!["7", "6", "4", "3", "1"]),
            ("burn,etch", vec!["8", "0"]),
            ("mint,mint", vec!["2"]),
            (
                "etch,mint,transfer,burn",
                vec!["8", "7", "6", "4", "3", "2", "1", "0"],
            ),
        ] {
            let paged = pages(
                &app,
                &format!(
                    "/v1/runes/rune/events?rune=10%3A1&category={}&limit=2",
                    encode(category)
                ),
                "events",
                "before_id",
            )
            .await;
            assert_eq!(ids(&paged, "id").concat(), expected, "{category}");
        }
        let page = result(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&category=mint%2Cmint%2Cburn",
        )
        .await;
        assert_eq!(page["category"], "mint,burn");
        // with type, an event passes both
        let both = result(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&category=transfer&type=input",
        )
        .await;
        assert_eq!(
            ids(&[both["events"].as_array().unwrap().clone()], "id"),
            strings(&[&["3"]])
        );
        let neither = result(
            &app,
            "/v1/runes/rune/events?rune=10%3A1&category=mint&type=input",
        )
        .await;
        assert_eq!(neither["events"], json!([]));
        assert_eq!(neither["has_more"], false);
        let categories =
            "category must be one or more of etch, mint, transfer, burn, comma-separated";
        for bad_category in ["input", "mint%2C", "Mint", "%2C"] {
            expect(
                &app,
                &format!("/v1/runes/rune/events?rune=10%3A1&category={bad_category}"),
                400,
                JSON,
                &bad(categories),
            )
            .await;
        }

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
            &format!("/v1/runes/wallet/events?address={}&limit=2", addr(1)),
            "events",
            "before_id",
        )
        .await;
        assert_eq!(ids(&wallet, "id"), strings(&[&["6", "5"], &["4"]]));
        assert_eq!(wallet[0][1]["rune_id"], "30:2");
        assert_eq!(wallet[0][1]["divisibility"], 8);
        assert_eq!(wallet[0][1]["address"], addr(1));
        let by_pkscript = result(
            &app,
            &format!("/v1/runes/wallet/events?pkscript={}&type=input", pk(2)),
        )
        .await;
        assert_eq!(by_pkscript["pkscript"], pk(2));
        assert_eq!(by_pkscript["event_type"], "input");
        assert_eq!(by_pkscript["rune_id"], Value::Null);
        assert_eq!(
            ids(&[by_pkscript["events"].as_array().unwrap().clone()], "id"),
            strings(&[&["3"]])
        );
        assert_eq!(by_pkscript["events"][0]["outpoint"], op(2, 1));
        let none = result(&app, "/v1/runes/wallet/events?address=nobody").await;
        assert_eq!(none["events"], json!([]));

        // one rune of a wallet, by id or name, with the other filters
        for rune in ["30%3A2", "uncommongifts"] {
            let page = result(
                &app,
                &format!("/v1/runes/wallet/events?address={}&rune={rune}", addr(1)),
            )
            .await;
            assert_eq!(page["rune_id"], "30:2");
            assert_eq!(
                ids(&[page["events"].as_array().unwrap().clone()], "id"),
                strings(&[&["5"]])
            );
        }
        let paged = pages(
            &app,
            &format!(
                "/v1/runes/wallet/events?pkscript={}&rune=FIRSTRUNE&limit=1",
                pk(2)
            ),
            "events",
            "before_id",
        )
        .await;
        assert_eq!(ids(&paged, "id"), strings(&[&["7"], &["3"], &["1"]]));
        let outputs = result(
            &app,
            &format!(
                "/v1/runes/wallet/events?pkscript={}&rune=10%3A1&category=transfer&type=output",
                pk(2)
            ),
        )
        .await;
        assert_eq!(outputs["category"], "transfer");
        assert_eq!(
            ids(&[outputs["events"].as_array().unwrap().clone()], "id"),
            strings(&[&["7", "1"]])
        );
        let other = result(
            &app,
            &format!("/v1/runes/wallet/events?pkscript={}&rune=20%3A1", pk(2)),
        )
        .await;
        assert_eq!(other["events"], json!([]));
        expect(
            &app,
            &format!("/v1/runes/wallet/events?pkscript={}&rune=NOSUCHRUNE", pk(2)),
            404,
            JSON,
            NOT_FOUND,
        )
        .await;
        expect(
            &app,
            &format!("/v1/runes/wallet/events?pkscript={}&rune=A..B", pk(2)),
            400,
            JSON,
            &bad("rune must be a rune id (block:tx) or a rune name (letters A-Z and spacers)"),
        )
        .await;
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

#[tokio::test]
async fn the_etchings_list_newest_first() {
    with_runes("etchings", |app, _| async move {
        let all = result(&app, "/v1/runes/etchings").await;
        assert_eq!(all["has_more"], false);
        assert_eq!(all["next_before_number"], Value::Null);
        assert_eq!(all["limit"], 100);
        assert_eq!(all["block_height"], 40);
        let listed: Vec<&str> = all["runes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rune| rune["rune_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed, ["30:2", "20:1", "10:1", "1:0"]);
        // each entry is the rune object of /v1/runes/rune
        let mut rune = result(&app, "/v1/runes/rune?rune=10%3A1").await;
        rune.as_object_mut().unwrap().remove("block_height");
        assert_eq!(all["runes"][2], rune);

        let paged = pages(&app, "/v1/runes/etchings?limit=3", "runes", "before_number").await;
        assert_eq!(
            ids(&paged, "rune_id"),
            strings(&[&["30:2", "20:1", "10:1"], &["1:0"]])
        );
        let first = result(&app, "/v1/runes/etchings?limit=3").await;
        assert_eq!(first["next_before_number"], 1);
        let older = result(&app, "/v1/runes/etchings?before_number=2").await;
        assert_eq!(
            ids(&[older["runes"].as_array().unwrap().clone()], "rune_id"),
            strings(&[&["10:1", "1:0"]])
        );
        for (uri, reason) in [
            (
                "/v1/runes/etchings?before_number=0",
                "before_number must be an integer from 1 to 999999999999999999",
            ),
            (
                "/v1/runes/etchings?before_number=x",
                "before_number must be an integer from 1 to 999999999999999999",
            ),
            (
                "/v1/runes/etchings?limit=501",
                "limit must be an integer from 1 to 500",
            ),
        ] {
            expect(&app, uri, 400, JSON, &bad(reason)).await;
        }
    })
    .await;
}

/// The parity routes print stored bytes as the JavaScript API's text
/// columns held them, with the address derived on the database's network.
#[tokio::test]
async fn the_parity_routes_print_hex_outpoints_and_addresses() {
    with_runes("parity", |app, _| async move {
        let event = answer(&app, &format!("/v1/runes/event?transaction_id={}", tx(0x12)), 200).await;
        let events = event["result"].as_array().unwrap();
        assert_eq!(events.len(), 6);
        assert_eq!(
            events[0],
            json!({"event_type": "input", "outpoint": op(2, 1), "pkscript": pk(2),
                   "wallet_addr": addr(2), "rune_id": "10:1", "amount": "1000"})
        );
        assert_eq!(
            events[5],
            json!({"event_type": "burn", "outpoint": null, "pkscript": null,
                   "wallet_addr": null, "rune_id": "10:1", "amount": "7"})
        );
        // a transaction id matches its lowercase hex only, as a text match did
        for other in [format!("{}0", tx(0x12)), "zz".into(), tx(0x13)] {
            let none = answer(&app, &format!("/v1/runes/event?transaction_id={other}"), 200).await;
            assert_eq!(none["result"], json!([]), "{other}");
        }
        let activity = answer(&app, "/v1/runes/activity_on_block?block_height=11", 200).await;
        assert_eq!(
            activity["result"],
            json!([
                {"event_type": "output", "outpoint": op(2, 1), "pkscript": pk(2), "rune_id": "10:1", "amount": "1000"},
                {"event_type": "mint", "outpoint": null, "pkscript": null, "rune_id": "10:1", "amount": "50"},
            ])
        );

        let by_address = answer(
            &app,
            &format!("/v1/runes/get_unspent_rune_outpoints_of_wallet?address={}", addr(2)),
            200,
        )
        .await;
        let mut outpoints: Vec<Value> = by_address["result"].as_array().unwrap().clone();
        outpoints.sort_by_key(|o| o["outpoint"].as_str().unwrap().to_string());
        assert_eq!(
            outpoints,
            [
                json!({"pkscript": pk(2), "wallet_addr": addr(2), "outpoint": op(2, 0), "rune_ids": ["10:1"], "balances": [10]}),
                json!({"pkscript": pk(2), "wallet_addr": addr(2), "outpoint": op(2, 2), "rune_ids": ["20:1"], "balances": [5]}),
            ]
        );
        let by_pkscript = answer(
            &app,
            &format!("/v1/runes/get_unspent_rune_outpoints_of_wallet?pkscript={}", pk(2)),
            200,
        )
        .await;
        assert_eq!(by_pkscript["result"].as_array().unwrap().len(), 2);
        // a pkscript matches its lowercase hex only, and a script without
        // an address answers a null wallet_addr
        let lower = answer(&app, "/v1/runes/get_unspent_rune_outpoints_of_wallet?pkscript=ffff", 200).await;
        assert_eq!(lower["result"][0]["wallet_addr"], Value::Null);
        assert_eq!(lower["result"][0]["outpoint"], op(8, 0));
        let upper = answer(&app, "/v1/runes/get_unspent_rune_outpoints_of_wallet?pkscript=FFFF", 200).await;
        assert_eq!(upper["result"], json!([]));

        // holder 2 at the start of 12 still holds the outpoint 12 spends
        let balance = answer(
            &app,
            &format!("/v1/runes/balance_on_block?block_height=12&address={}", addr(2)),
            200,
        )
        .await;
        assert_eq!(
            balance["result"],
            json!([{"pkscript": pk(2), "wallet_addr": addr(2), "rune_id": "10:1",
                    "rune_name": "FIRSTRUNE", "total_balance": "1000"}])
        );
        let current = answer(
            &app,
            &format!("/v1/runes/get_current_balance_of_wallet?address={}", addr(1)),
            200,
        )
        .await;
        let mut balances: Vec<(String, String)> = current["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                assert_eq!(b["wallet_addr"], addr(1));
                (b["rune_id"].as_str().unwrap().into(), b["total_balance"].as_str().unwrap().into())
            })
            .collect();
        balances.sort();
        assert_eq!(
            balances,
            [("10:1".into(), "30".into()), ("1:0".into(), "2".into()), ("30:2".into(), "10".into())]
        );
        let nobody = answer(&app, "/v1/runes/get_current_balance_of_wallet?address=nobody", 200).await;
        assert_eq!(nobody["result"], json!([]));
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
         where o.spent_block_height is null and u.balance > 0 group by 1, 2 order by 3 desc, 1 limit 5",
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
    let (wallet, pkscript) = live_runes_wallet(&pool).await;
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
    let count: i64 =
        sqlx::query_scalar("select count(*) from runes_events where pkscript = decode($1, 'hex')")
            .bind(&pkscript)
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
