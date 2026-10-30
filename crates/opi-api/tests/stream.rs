//! The push feed `/v1/stream` over HTTP.
//!
//! The parameter tests use a database double: parameters are checked before
//! any query. The seeded tests build one throwaway database from the brc20,
//! bitmap and sns schemas on the local Postgres (127.0.0.1:5432, the current
//! user), add, rewrite and drop blocks the way the indexer does, and check
//! every frame's data against the document's schema of its event; they skip
//! unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{Frame, JSON, Sse, expect, get, height, with_seeded_pool},
    opi_api::{
        config::{Module, load_config},
        db::Db,
        router, serve_listeners,
    },
    serde_json::{Value, json},
    sqlx::PgPool,
    std::{collections::HashMap, time::Duration},
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    },
};

fn bad(reason: &str) -> String {
    format!(r#"{{"error":"{reason}","result":null}}"#)
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = height(Module::Brc20, 7.into());
    for (uri, reason) in [
        (
            "/v1/stream?modules=runes",
            "runes is not enabled on this node",
        ),
        ("/v1/stream?modules=sns", "sns is not enabled on this node"),
        (
            "/v1/stream?modules=ord",
            "modules names an unknown module: ord",
        ),
        (
            "/v1/stream?confirmations=0",
            "confirmations must be an integer from 1 to 100",
        ),
        (
            "/v1/stream?confirmations=101",
            "confirmations must be an integer from 1 to 100",
        ),
        (
            "/v1/stream?from_height=x",
            "from_height must be an integer from 0 to 2147483647",
        ),
        (
            "/v1/stream?from_height=1&from_height=2",
            "from_height must be a single value",
        ),
    ] {
        expect(&app, uri, 400, JSON, &bad(reason)).await;
    }
    let runes = common::app_with_env(
        Module::Runes,
        Db::fake(|_| Ok(7.into())),
        &[("OPI_MODULES", "brc20,runes"), ("DB_DATABASE", "d")],
    );
    expect(
        &runes,
        "/v1/stream?modules=brc20,runes",
        400,
        JSON,
        &bad("runes is not on the stream yet"),
    )
    .await;
}

#[tokio::test]
async fn a_stream_whose_setup_query_fails_answers_500() {
    let app = height(Module::Brc20, 7.into());
    expect(&app, "/v1/stream", 500, JSON, common::INTERNAL_ERROR).await;
}

/// One token in raw units.
const E18: &str = "000000000000000000";

const MODULES: [&str; 3] = ["bitmap", "brc20", "sns"];

/// The rows of `module`'s block `height` on the branch `tag`: its hash
/// `<tag><height>`, and `events_of(height)` brc20 mints of ordi, one
/// bitmap or one sns name.
fn module_block(module: &str, height: i32, tag: &str) -> String {
    match module {
        "bitmap" => format!(
            "INSERT INTO bitmap_block_hashes (block_height, block_hash) VALUES ({height}, '{tag}{height}');
             INSERT INTO bitmaps (id, inscription_id, inscription_number, bitmap_number, block_height)
                 VALUES ({height}, '{tag}bm{height}i0', {height}, {height}, {height});"
        ),
        "sns" => format!(
            "INSERT INTO sns_block_hashes (block_height, block_hash) VALUES ({height}, '{tag}{height}');
             INSERT INTO sns_names (id, inscription_id, inscription_number, \"name\", domain, block_height)
                 VALUES ({height}, '{tag}sn{height}i0', {height}, '{tag}{height}.sats', 'sats', {height});"
        ),
        "brc20" => {
            let mut sql = format!(
                "INSERT INTO brc20_block_hashes (block_height, block_hash, block_time)
                     VALUES ({height}, '{tag}{height}', {});",
                1000 + height
            );
            for i in 0..events_of(height) {
                let id = i64::from(height) * 1000 + i64::from(i);
                sql.push_str(&format!(
                    "INSERT INTO brc20_events (id, event_type, block_height, inscription_id, inscription_number,
                                               old_satpoint, new_satpoint, txid, event)
                         VALUES ({id}, 1, {height}, '{tag}m{id}i0', {id}, NULL, 'sp', 'tx',
                                 '{{\"minted_pkScript\":\"0014a1\",\"minted_wallet\":\"tb1q{tag}\",\"tick\":\"ordi\",\"original_tick\":\"ORDI\",\"amount\":\"{i}{E18}\",\"parent_id\":\"\"}}');"
                ));
            }
            sql
        }
        other => panic!("no module {other}"),
    }
}

/// The events of a brc20 block at `height`.
fn events_of(height: i32) -> i32 {
    if height == 3 { 250 } else { 2 }
}

async fn add_blocks(pool: &PgPool, heights: std::ops::RangeInclusive<i32>, tag: &str) {
    add_module_blocks(pool, &MODULES, heights, tag).await;
}

/// Adds `modules`' blocks `heights` of branch `tag`; the indexer commits
/// each block with its hash row.
async fn add_module_blocks(
    pool: &PgPool,
    modules: &[&str],
    heights: std::ops::RangeInclusive<i32>,
    tag: &str,
) {
    for height in heights {
        let rows: String = modules
            .iter()
            .map(|module| module_block(module, height, tag))
            .collect();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {rows} COMMIT;")))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// A reorg as the indexer runs it: every module's rows above `kept` go in
/// one transaction, then the blocks of branch `tag` up to `to` come.
async fn reorg(pool: &PgPool, kept: i32, to: i32, tag: &str) {
    reorg_modules(pool, &MODULES, kept, to, tag).await;
}

/// [`reorg`] of `modules` alone.
async fn reorg_modules(pool: &PgPool, modules: &[&str], kept: i32, to: i32, tag: &str) {
    let deletes: String = modules
        .iter()
        .map(|module| {
            let rows = match *module {
                "bitmap" => "bitmaps",
                "sns" => "sns_names",
                _ => "brc20_events",
            };
            format!(
                "DELETE FROM {rows} WHERE block_height > {kept};
                 DELETE FROM {module}_block_hashes WHERE block_height > {kept};"
            )
        })
        .collect();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {deletes} COMMIT;")))
        .execute(pool)
        .await
        .unwrap();
    add_module_blocks(pool, modules, kept + 1..=to, tag).await;
}

/// The entry of `module` in a stream id.
fn entry<'a>(id: &'a str, module: &str) -> Option<&'a str> {
    id.split(',')
        .find(|entry| entry.split(':').next() == Some(module))
}

/// The app of brc20, bitmap and sns over `pool`, with the `vars` settings.
fn stream_app(pool: &PgPool, vars: &[(&str, &str)]) -> Router {
    let env: HashMap<String, String> = [
        ("OPI_MODULES", "brc20,bitmap,sns"),
        ("DB_DATABASE", "d"),
        ("BITMAP_DB_DATABASE", "d"),
        ("SNS_DB_DATABASE", "d"),
        ("STREAM_POLL_MS", "50"),
    ]
    .into_iter()
    .chain(vars.iter().copied())
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let config = load_config(|key| env.get(key).cloned()).unwrap();
    let db = Db::from_pool(pool.clone());
    router(
        &config,
        vec![
            (Module::Brc20, db.clone()),
            (Module::Bitmap, db.clone()),
            (Module::Sns, db),
        ],
    )
}

const BRC20_TICKER: &str = "INSERT INTO brc20_tickers (original_tick, tick, max_supply, decimals,
        limit_per_mint, remaining_supply, burned_supply, is_self_mint, deploy_inscription_id, block_height)
    VALUES ('ORDI', 'ordi', 21000000, 18, 1000, 1000, 0, false, 'ordii0', 1);";

/// Runs `body` on a throwaway database with the three schemas, the ticker
/// ordi and the blocks 1 to `blocks` of branch `b`.
async fn with_chain<F, Fut>(tag: &str, blocks: i32, body: F)
where
    F: FnOnce(PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_chain_pool(tag, blocks, 10, body).await;
}

/// [`with_chain`] on a pool of `max_connections`.
async fn with_chain_pool<F, Fut>(tag: &str, blocks: i32, max_connections: u32, body: F)
where
    F: FnOnce(PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let scripts = [
        include_str!("../../opi-brc20/src/database/sql/db_init.sql"),
        include_str!("../../opi-bitmap/src/db_init.sql"),
        include_str!("../../opi-sns/src/db_init.sql"),
        BRC20_TICKER,
    ];
    with_seeded_pool(
        Module::Brc20,
        tag,
        None,
        &scripts,
        max_connections,
        |_, pool| async move {
            add_blocks(&pool, 1..=blocks, "b").await;
            body(pool).await;
        },
    )
    .await;
}

/// The next frame that is not a heartbeat, which must be there.
async fn message(sse: &mut Sse) -> Frame {
    sse.next_message().await.expect("a message")
}

/// Takes the next frame as block `height` of `module` on branch `tag`.
async fn expect_block(sse: &mut Sse, module: &str, height: i32, tag: &str) -> Frame {
    let frame = message(sse).await;
    assert_eq!(frame.event, "block", "{frame:?}");
    let hash = format!("{tag}{height}");
    let id = frame.id.as_deref().expect("a block's id");
    assert_eq!(
        entry(id, module),
        Some(format!("{module}:{height}:{hash}").as_str()),
        "{frame:?}"
    );
    assert_eq!(frame.data["module"], module);
    assert_eq!(frame.data["height"], height);
    assert_eq!(frame.data["hash"], hash);
    frame
}

/// Takes the next frame as the rollback of `module` to `height`, whose
/// block is on branch `tag` (`None` for a height without a stored block).
/// Answers the rollback's id.
async fn expect_rollback(sse: &mut Sse, module: &str, height: i32, tag: Option<&str>) -> String {
    let frame = message(sse).await;
    assert_eq!(frame.event, "rollback", "{frame:?}");
    assert_eq!(frame.data, json!({"module": module, "height": height}));
    let id = frame.id.expect("a rollback's id");
    let hash = tag.map(|tag| format!("{tag}{height}")).unwrap_or_default();
    assert_eq!(
        entry(&id, module),
        Some(format!("{module}:{height}:{hash}").as_str()),
        "{id}"
    );
    id
}

/// The blocks a stream's frames leave the client with: each block's
/// height, hash and events after applying the rollbacks, and the
/// rollbacks' heights.
#[derive(Default)]
struct Applied {
    blocks: Vec<(i32, String, Vec<Value>)>,
    rollbacks: Vec<i32>,
}

impl Applied {
    fn apply(&mut self, frame: &Frame) {
        let height = frame.data["height"].as_i64().unwrap() as i32;
        match frame.event.as_str() {
            "block" => {
                if let Some((last, ..)) = self.blocks.last() {
                    assert_eq!(*last + 1, height, "blocks in order");
                }
                self.blocks.push((
                    height,
                    frame.data["hash"].as_str().unwrap().to_string(),
                    frame.data["events"].as_array().unwrap().clone(),
                ));
            }
            "rollback" => {
                self.rollbacks.push(height);
                self.blocks.retain(|(h, ..)| *h <= height);
            }
            other => panic!("an unexpected {other}"),
        }
    }

    fn events(&self) -> Vec<Value> {
        self.blocks
            .iter()
            .flat_map(|(_, _, events)| events.clone())
            .collect()
    }
}

/// Seeds the brc20 blocks `heights` of branch `b` with `events` events of
/// about 500 bytes each.
async fn add_big_blocks(pool: &PgPool, heights: std::ops::RangeInclusive<i32>, events: i32) {
    let (low, high) = (heights.start(), heights.end());
    let sql = format!(
        "INSERT INTO brc20_block_hashes (block_height, block_hash, block_time)
             SELECT h, 'b' || h, 1000 + h FROM generate_series({low}, {high}) h;
         INSERT INTO brc20_events (id, event_type, block_height, inscription_id, inscription_number,
                                   old_satpoint, new_satpoint, txid, event)
             SELECT h * 100000 + i, 1, h, 'bm' || (h * 100000 + i) || 'i0', h * 100000 + i,
                    NULL, 'sp', 'tx',
                    jsonb_build_object('minted_pkScript', '0014a1', 'minted_wallet', 'tb1qb',
                        'tick', 'ordi', 'original_tick', 'ORDI', 'amount', i || '{E18}',
                        'parent_id', repeat('p', 300))
             FROM generate_series({low}, {high}) h, generate_series(0, {events} - 1) i;"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await
        .unwrap();
}

/// The brc20 events the event feed lists, oldest first.
async fn feed(app: &Router) -> Vec<Value> {
    let answer = get(app, "/v1/brc20/ticker/events?ticker=ordi&limit=500").await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    let mut events = body["result"]["events"].as_array().unwrap().clone();
    events.reverse();
    events
}

#[tokio::test]
async fn six_confirmations_deliver_each_block_once_in_order() {
    with_chain("stream_conf", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(
            &app,
            "/v1/stream?modules=brc20&confirmations=6&from_height=1",
            None,
        )
        .await;
        for height in 1..=5 {
            let frame = expect_block(&mut sse, "brc20", height, "b").await;
            assert_eq!(frame.data["block_time"], 1000 + height);
            assert_eq!(frame.data["decimals"], 18);
            let events = frame.data["events"].as_array().unwrap();
            assert_eq!(events.len(), events_of(height) as usize);
            let ids: Vec<i64> = events.iter().map(|e| e["id"].as_i64().unwrap()).collect();
            let mut sorted = ids.clone();
            sorted.sort();
            assert_eq!(ids, sorted);
        }
        for height in 11..=20 {
            add_blocks(&pool, height..=height, "b").await;
            expect_block(&mut sse, "brc20", height - 5, "b").await;
        }
        // nothing more is due: only heartbeats could follow
        let quiet = tokio::time::timeout(Duration::from_millis(500), sse.next_message()).await;
        assert!(quiet.is_err(), "{quiet:?}");
    })
    .await;
}

#[tokio::test]
async fn a_reorg_rolls_a_one_confirmation_subscriber_back() {
    with_chain("stream_reorg", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20&from_height=1", None).await;
        let mut delivered: Vec<(i32, Vec<Value>)> = Vec::new();
        for height in 1..=10 {
            let frame = expect_block(&mut sse, "brc20", height, "b").await;
            delivered.push((height, frame.data["events"].as_array().unwrap().clone()));
        }
        reorg(&pool, 7, 12, "x").await;
        expect_rollback(&mut sse, "brc20", 7, Some("b")).await;
        delivered.retain(|(height, _)| *height <= 7);
        for height in 8..=12 {
            let frame = expect_block(&mut sse, "brc20", height, "x").await;
            delivered.push((height, frame.data["events"].as_array().unwrap().clone()));
        }
        let streamed: Vec<Value> = delivered
            .into_iter()
            .flat_map(|(_, events)| events)
            .collect();
        assert_eq!(streamed, feed(&app).await);
    })
    .await;
}

#[tokio::test]
async fn a_stream_resumes_by_from_height_and_by_last_event_id() {
    with_chain("stream_resume", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20&from_height=4", None).await;
        expect_block(&mut sse, "brc20", 4, "b").await;
        drop(sse);
        // the header wins over from_height
        let mut sse = Sse::open(
            &app,
            "/v1/stream?modules=brc20&from_height=2",
            Some("brc20:6:b6"),
        )
        .await;
        expect_block(&mut sse, "brc20", 7, "b").await;
        drop(sse);

        // a stale id, with the reorg seen by the poller another stream keeps
        let mut watching = Sse::open(&app, "/v1/stream?modules=brc20&from_height=10", None).await;
        expect_block(&mut watching, "brc20", 10, "b").await;
        reorg(&pool, 5, 10, "x").await;
        expect_rollback(&mut watching, "brc20", 5, Some("b")).await;
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20", Some("brc20:6:b6")).await;
        expect_rollback(&mut sse, "brc20", 5, Some("b")).await;
        for height in 6..=10 {
            expect_block(&mut sse, "brc20", height, "x").await;
        }
        drop((sse, watching));

        // a stale id this process saw no reorg of rolls back as far as a
        // reorg may go
        let fresh = stream_app(&pool, &[]);
        let mut sse = Sse::open(&fresh, "/v1/stream?modules=brc20", Some("brc20:6:b6")).await;
        expect_rollback(&mut sse, "brc20", 0, None).await;
        expect_block(&mut sse, "brc20", 1, "b").await;
        drop(sse);

        // a rollback's id resumes after the kept block
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20", Some("brc20:5:b5")).await;
        expect_block(&mut sse, "brc20", 6, "x").await;
    })
    .await;
}

#[tokio::test]
async fn a_stale_id_rolls_back_every_module_of_the_stream() {
    with_chain("stream_stale_modules", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        // a stream at the tip keeps the pollers, which see the reorg
        let mut watching = Sse::open(&app, "/v1/stream?from_height=10", None).await;
        for module in ["bitmap", "brc20", "sns"] {
            expect_block(&mut watching, module, 10, "b").await;
        }
        reorg(&pool, 5, 10, "x").await;
        // each module's poller sees the reorg on its own
        let mut rolled = Vec::new();
        while rolled.len() < 3 {
            let frame = message(&mut watching).await;
            if frame.event == "rollback" {
                assert_eq!(frame.data["height"], 5);
                rolled.push(frame.data["module"].as_str().unwrap().to_string());
            }
        }
        rolled.sort();
        assert_eq!(rolled, ["bitmap", "brc20", "sns"]);
        // each module of the id resumes from its own position
        let stale = "bitmap:6:b6,brc20:6:b6,sns:5:b5";
        let mut sse = Sse::open(&app, "/v1/stream", Some(stale)).await;
        expect_rollback(&mut sse, "bitmap", 5, Some("b")).await;
        let id = expect_rollback(&mut sse, "brc20", 5, Some("b")).await;
        assert_eq!(id, "bitmap:5:b5,brc20:5:b5,sns:5:b5");
        for height in 6..=10 {
            for module in MODULES {
                expect_block(&mut sse, module, height, "x").await;
            }
        }
        drop((sse, watching));
        // a fresh process, which saw no reorg, rolls each module back as
        // deep as a reorg may go
        let fresh = stream_app(&pool, &[]);
        let mut sse = Sse::open(
            &fresh,
            "/v1/stream",
            Some("bitmap:6:b6,brc20:6:b6,sns:6:b6"),
        )
        .await;
        for module in MODULES {
            expect_rollback(&mut sse, module, 0, None).await;
        }
        expect_block(&mut sse, "bitmap", 1, "b").await;
    })
    .await;
}

#[tokio::test]
async fn an_id_taken_between_modules_rollbacks_resumes_each_module() {
    with_chain("stream_between", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", None).await;
        for height in 1..=10 {
            for module in MODULES {
                expect_block(&mut sse, module, height, "b").await;
            }
        }
        // bitmap reorgs first; the client leaves after its rollback
        reorg_modules(&pool, &["bitmap"], 5, 10, "x").await;
        let id = expect_rollback(&mut sse, "bitmap", 5, Some("b")).await;
        assert_eq!(id, "bitmap:5:b5,brc20:10:b10,sns:10:b10");
        drop(sse);
        // another stream keeps the pollers, which see the other reorgs
        let _watching = Sse::open(&app, "/v1/stream", None).await;
        reorg_modules(&pool, &["brc20", "sns"], 5, 10, "x").await;
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", Some(&id)).await;
        // Each module rolls back once its poller has read its reorg, while
        // bitmap, which needs none, may go on meanwhile. The client starts
        // from the id's state: blocks 1 to 5 of bitmap, 1 to 10 of the rest.
        let mut applied: HashMap<&str, Applied> = HashMap::new();
        for module in MODULES {
            let held = if module == "bitmap" { 5 } else { 10 };
            applied.entry(module).or_default().blocks = (1..=held)
                .map(|h| (h, format!("b{h}"), Vec::new()))
                .collect();
        }
        let done = |applied: &HashMap<&str, Applied>| {
            MODULES.iter().all(|module| {
                applied[module]
                    .blocks
                    .last()
                    .is_some_and(|(h, hash, _)| *h == 10 && hash == "x10")
            })
        };
        while !done(&applied) {
            let frame = message(&mut sse).await;
            let module = MODULES
                .into_iter()
                .find(|m| frame.data["module"] == *m)
                .unwrap();
            applied.get_mut(module).unwrap().apply(&frame);
        }
        for module in MODULES {
            let expected: &[i32] = if module == "bitmap" { &[] } else { &[5] };
            assert_eq!(applied[module].rollbacks, expected, "{module}");
            let hashes: Vec<&str> = applied[module]
                .blocks
                .iter()
                .map(|(_, hash, _)| hash.as_str())
                .collect();
            let want: Vec<String> = (1..=10)
                .map(|h| format!("{}{h}", if h <= 5 { "b" } else { "x" }))
                .collect();
            assert_eq!(hashes, want, "{module}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_position_waited_for_past_the_resume_wait_is_stale() {
    with_chain("stream_stale_wait", 10, |pool| async move {
        let app = stream_app(
            &pool,
            &[
                ("STREAM_RESUME_WAIT_SECS", "2"),
                ("STREAM_HEARTBEAT_SECS", "1"),
            ],
        );
        // this node's brc20 never reaches the id's block
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20", Some("brc20:12:b12")).await;
        let quiet = tokio::time::timeout(Duration::from_millis(1500), sse.next_message()).await;
        assert!(quiet.is_err(), "{quiet:?}");
        expect_rollback(&mut sse, "brc20", 0, None).await;
        expect_block(&mut sse, "brc20", 1, "b").await;
    })
    .await;
}

#[tokio::test]
async fn an_id_above_a_module_s_tip_waits_for_it() {
    with_chain("stream_ahead", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        // this node's brc20 is two blocks behind the id
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20", Some("brc20:12:b12")).await;
        let quiet = tokio::time::timeout(Duration::from_millis(500), sse.next_message()).await;
        assert!(quiet.is_err(), "{quiet:?}");
        add_module_blocks(&pool, &["brc20"], 11..=13, "b").await;
        expect_block(&mut sse, "brc20", 13, "b").await;
        // a position below the oldest kept block answers 400
        sqlx::raw_sql("DELETE FROM brc20_block_hashes WHERE block_height < 4;")
            .execute(&pool)
            .await
            .unwrap();
        let answer = common::get_with(
            &app,
            "/v1/stream?modules=brc20",
            &[("last-event-id", "brc20:3:b3")],
        )
        .await;
        assert_eq!(answer.status, 400);
        assert_eq!(
            answer.body,
            bad("Last-Event-ID is below 4, the oldest block brc20 keeps")
        );
    })
    .await;
}

#[tokio::test]
async fn a_reorg_during_a_replay_rolls_back_what_was_sent() {
    with_chain("stream_midreplay", 30, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20&from_height=1", None).await;
        let mut applied = Applied::default();
        for height in 1..=10 {
            applied.apply(&expect_block(&mut sse, "brc20", height, "b").await);
        }
        reorg(&pool, 5, 30, "x").await;
        // blocks of the old branch may still come before the rollback
        while applied
            .blocks
            .last()
            .is_none_or(|(height, hash, _)| !(*height == 30 && hash == "x30"))
        {
            applied.apply(&message(&mut sse).await);
        }
        assert_eq!(applied.rollbacks, [5]);
        assert!(
            applied.blocks[5..]
                .iter()
                .all(|(_, hash, _)| hash.starts_with('x'))
        );
        assert_eq!(applied.events(), feed(&app).await);
    })
    .await;
}

#[tokio::test]
async fn a_reorg_below_the_confirmations_rolls_back_and_one_above_them_does_not() {
    with_chain("stream_conf_reorg", 10, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(
            &app,
            "/v1/stream?modules=brc20&confirmations=3&from_height=1",
            None,
        )
        .await;
        for height in 1..=8 {
            expect_block(&mut sse, "brc20", height, "b").await;
        }
        reorg(&pool, 6, 12, "x").await;
        expect_rollback(&mut sse, "brc20", 6, Some("b")).await;
        for height in 7..=10 {
            expect_block(&mut sse, "brc20", height, "x").await;
        }
        // blocks 11 and 12 were not sent yet: replacing them needs no rollback
        reorg(&pool, 10, 13, "y").await;
        expect_block(&mut sse, "brc20", 11, "y").await;
    })
    .await;
}

#[tokio::test]
async fn several_modules_interleave_by_height_then_name() {
    with_chain("stream_modules", 3, |pool| async move {
        let app = stream_app(&pool, &[]);
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", None).await;
        for height in 1..=3 {
            for module in ["bitmap", "brc20", "sns"] {
                let frame = expect_block(&mut sse, module, height, "b").await;
                if module != "brc20" {
                    // the lookup routes' form of the block's registration
                    assert_eq!(frame.data["block_time"], Value::Null);
                    let events = frame.data["events"].as_array().unwrap();
                    assert_eq!(events.len(), 1);
                    let short = if module == "bitmap" { "bm" } else { "sn" };
                    let uri =
                        format!("/v1/{module}/by_inscription?inscription_id=b{short}{height}i0");
                    let answer: Value = serde_json::from_str(&get(&app, &uri).await.body).unwrap();
                    assert_eq!(events[0], answer["result"]);
                }
            }
        }
        // a resume within a height goes on with the modules after it
        drop(sse);
        let mut sse = Sse::open(
            &app,
            "/v1/stream?modules=sns,bitmap,brc20",
            Some("bitmap:2:b2,brc20:2:b2,sns:1:b1"),
        )
        .await;
        let frame = expect_block(&mut sse, "sns", 2, "b").await;
        assert_eq!(frame.id.as_deref(), Some("bitmap:2:b2,brc20:2:b2,sns:2:b2"));
        expect_block(&mut sse, "bitmap", 3, "b").await;
        // a module the id leaves out starts at from_height
        drop(sse);
        let mut sse = Sse::open(
            &app,
            "/v1/stream?modules=sns,brc20&from_height=1",
            Some("brc20:2:b2"),
        )
        .await;
        expect_block(&mut sse, "sns", 1, "b").await;
        let frame = expect_block(&mut sse, "sns", 2, "b").await;
        assert_eq!(frame.id.as_deref(), Some("brc20:2:b2,sns:2:b2"));
        expect_block(&mut sse, "brc20", 3, "b").await;
        // a reorg of one module rolls that module back alone
        drop(sse);
        let mut sse = Sse::open(&app, "/v1/stream?modules=sns,bitmap&from_height=1", None).await;
        for height in 1..=3 {
            expect_block(&mut sse, "bitmap", height, "b").await;
            expect_block(&mut sse, "sns", height, "b").await;
        }
        sqlx::raw_sql(
            "BEGIN; DELETE FROM sns_names WHERE block_height > 1;
             DELETE FROM sns_block_hashes WHERE block_height > 1;
             INSERT INTO sns_block_hashes (block_height, block_hash) VALUES (2, 'x2'), (3, 'x3');
             COMMIT;",
        )
        .execute(&pool)
        .await
        .unwrap();
        expect_rollback(&mut sse, "sns", 1, Some("b")).await;
        for height in 2..=3 {
            let frame = expect_block(&mut sse, "sns", height, "x").await;
            assert_eq!(frame.data["events"], json!([]));
        }
        let quiet = tokio::time::timeout(Duration::from_millis(500), sse.next_message()).await;
        assert!(quiet.is_err(), "{quiet:?}");
    })
    .await;
}

#[tokio::test]
async fn a_heartbeat_arrives_while_no_block_is_due() {
    with_chain("stream_heartbeat", 2, |pool| async move {
        let app = stream_app(&pool, &[("STREAM_HEARTBEAT_SECS", "1")]);
        let mut sse = Sse::open(&app, "/v1/stream", None).await;
        let frame = sse.next_within(Duration::from_secs(3)).await.unwrap();
        assert_eq!(frame.event, "heartbeat");
        assert_eq!(
            frame.data,
            json!({"tips": {"bitmap": 2, "brc20": 2, "sns": 2}})
        );
    })
    .await;
}

#[tokio::test]
async fn a_start_below_the_oldest_block_answers_400() {
    with_chain("stream_oldest", 4, |pool| async move {
        sqlx::raw_sql("DELETE FROM bitmap_block_hashes WHERE block_height < 3;")
            .execute(&pool)
            .await
            .unwrap();
        let app = stream_app(&pool, &[]);
        expect(
            &app,
            "/v1/stream?from_height=2",
            400,
            JSON,
            &bad("from_height is below 3, the oldest block bitmap keeps"),
        )
        .await;
        let mut sse = Sse::open(&app, "/v1/stream?modules=bitmap&from_height=3", None).await;
        expect_block(&mut sse, "bitmap", 3, "b").await;
    })
    .await;
}

#[tokio::test]
async fn the_subscriber_cap_answers_503() {
    with_chain("stream_cap", 2, |pool| async move {
        let app = stream_app(&pool, &[("STREAM_MAX_SUBSCRIBERS", "1")]);
        let sse = Sse::open(&app, "/v1/stream", None).await;
        expect(
            &app,
            "/v1/stream",
            503,
            JSON,
            &bad("too many stream subscribers"),
        )
        .await;
        drop(sse);
        // the stream ends once its client has gone, freeing its place
        wait_for_a_free_place(&app).await;
    })
    .await;
}

#[tokio::test]
async fn a_client_s_newer_stream_ends_its_oldest() {
    with_chain("stream_per_client", 2, |pool| async move {
        let app = stream_app(
            &pool,
            &[("STREAM_MAX_PER_CLIENT", "1"), ("STREAM_MAX_SUBSCRIBERS", "1")],
        );
        let uri = "/v1/stream?modules=brc20&from_height=1";
        let mut old = Sse::open(&app, uri, None).await;
        expect_block(&mut old, "brc20", 1, "b").await;
        // the newer stream takes the oldest's place, under the global cap too
        let mut new = Sse::open(&app, uri, None).await;
        expect_block(&mut new, "brc20", 1, "b").await;
        loop {
            let frame = old.next().await.expect("the error event");
            if frame.event == "error" {
                assert_eq!(
                    frame.data,
                    json!({"error": "a newer stream of this client took this stream's place (STREAM_MAX_PER_CLIENT)"})
                );
                break;
            }
        }
        assert_eq!(old.next().await, None);
    })
    .await;
}

/// Asks for a stream until one answers, within 10 seconds; the answer's
/// stream closes at once.
async fn wait_for_a_free_place(app: &Router) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let answer = get(app, "/v1/stream").await;
        if answer.status == 200 {
            return;
        }
        assert_eq!(answer.status, 503, "{}", answer.body);
        assert!(tokio::time::Instant::now() < deadline, "no place came free");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_slow_subscriber_is_cut_with_an_error_event() {
    with_chain("stream_slow", 6, |pool| async move {
        let app = stream_app(
            &pool,
            &[
                ("STREAM_BUFFER", "1"),
                ("STREAM_HEARTBEAT_SECS", "1"),
                ("STREAM_MAX_SUBSCRIBERS", "1"),
            ],
        );
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", None).await;
        // the stream, unread, ends and frees its place
        wait_for_a_free_place(&app).await;
        let frame = sse.next().await.unwrap();
        assert_eq!(frame.event, "error");
        assert_eq!(
            frame.data,
            json!({"error": "the client fell behind the stream; reconnect with Last-Event-ID to resume"})
        );
        assert_eq!(sse.next().await, None);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ends_the_streams() {
    with_chain("stream_shutdown", 2, |pool| async move {
        let env: HashMap<&str, &str> = [("OPI_MODULES", "brc20"), ("DB_DATABASE", "d")]
            .into_iter()
            .collect();
        let config = load_config(|key| env.get(key).map(|v| v.to_string())).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let done = tokio::spawn(serve_listeners(
            config,
            vec![(Module::Brc20, Db::from_pool(pool.clone()))],
            listener,
            None,
            async {
                stopped.await.ok();
            },
        ));
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /v1/stream?from_height=1 HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0; 4096];
        while !String::from_utf8_lossy(&seen).contains("brc20:2:b2") {
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(n > 0, "{}", String::from_utf8_lossy(&seen));
            seen.extend_from_slice(&buffer[..n]);
        }
        let head = String::from_utf8_lossy(&seen).to_string();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(
            head.contains("content-type: text/event-stream\r\n"),
            "{head}"
        );
        let started = std::time::Instant::now();
        stop.send(()).unwrap();
        // the stream ends: the chunked body's last chunk, then the close
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
                .await
                .expect("the stream ends")
                .unwrap();
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&buffer[..n]);
        }
        assert!(String::from_utf8_lossy(&seen).ends_with("0\r\n\r\n"));
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .expect("the API stops")
            .unwrap()
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    })
    .await;
}

/// The API over brc20's database on `pool`, with the `vars` settings, on a
/// local port, until the sender's drop or send.
async fn serve_brc20(
    pool: &PgPool,
    vars: &[(&str, &str)],
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let env: HashMap<&str, &str> = [("OPI_MODULES", "brc20"), ("DB_DATABASE", "d")]
        .into_iter()
        .chain(vars.iter().copied())
        .collect();
    let config = load_config(|key| env.get(key).map(|v| v.to_string())).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_listeners(
        config,
        vec![(Module::Brc20, Db::from_pool(pool.clone()))],
        listener,
        None,
        async {
            stopped.await.ok();
        },
    ));
    (addr, stop)
}

/// Opens a stream of `path` with `headers` once a place is free, within
/// 20 seconds, on a socket with a small receive buffer, so a client that
/// stops reading holds back the server soon. Answers the socket and the
/// body read with the head.
async fn open_free(addr: std::net::SocketAddr, path: &str, headers: &str) -> (TcpStream, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (stream, head, body) = open_raw(addr, path, headers).await;
        if head.starts_with("HTTP/1.1 200 OK") {
            return (stream, body);
        }
        assert!(head.starts_with("HTTP/1.1 503"), "{head}");
        assert!(tokio::time::Instant::now() < deadline, "no place came free");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Sends a GET of `path` with `headers` and reads the answer's head.
async fn open_raw(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &str,
) -> (TcpStream, String, Vec<u8>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut stream = socket.connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\n{headers}\r\n").as_bytes())
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        if let Some(end) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&seen[..end]).to_string();
            let rest = seen[end + 4..].to_vec();
            return (stream, head, rest);
        }
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0, "{}", String::from_utf8_lossy(&seen));
        seen.extend_from_slice(&buffer[..n]);
    }
}

/// The payload of a chunked body, read to its end.
fn unchunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .expect("a chunk size");
        let size =
            usize::from_str_radix(std::str::from_utf8(&body[..line_end]).unwrap(), 16).unwrap();
        body = &body[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_cut_off_mid_block_resumes_at_that_block() {
    with_chain("stream_cut", 0, |pool| async move {
        add_big_blocks(&pool, 1..=40, 1000).await;
        let (addr, _stop) = serve_brc20(
            &pool,
            &[
                ("STREAM_BUFFER", "1"),
                ("STREAM_HEARTBEAT_SECS", "1"),
                ("STREAM_MAX_SUBSCRIBERS", "1"),
            ],
        )
        .await;
        // A stream ends between frames about one time in eleven (a block is
        // a head, ten pages and a tail); a few tries give a cut frame.
        let mut text = None;
        for _ in 0..8 {
            let (mut stream, mut body) =
                open_free(addr, "/v1/stream?from_height=1", "connection: close\r\n").await;
            // read a little, then stop reading until the stream has ended
            let mut buffer = vec![0; 16 * 1024];
            let n = stream.read(&mut buffer).await.unwrap();
            body.extend_from_slice(&buffer[..n]);
            drop(open_free(addr, "/v1/stream", "connection: close\r\n").await);
            stream.read_to_end(&mut body).await.unwrap();
            let read = String::from_utf8(unchunk(&body)).unwrap();
            if !read.ends_with("\n\n") {
                text = Some(read);
                break;
            }
        }
        let text = text.expect("a stream cut in the middle of a frame");
        let (whole, cut) = text.rsplit_once("\n\n").unwrap();
        assert!(!cut.is_empty());
        assert!(
            !cut.contains("\nid: "),
            "an id in the unfinished frame {:.200}",
            cut
        );
        let mut last_id = None;
        for frame in whole.split("\n\n") {
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            let data: Value = serde_json::from_str(data).expect("a whole frame's data");
            if frame.starts_with("event: block") {
                last_id = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("id: "))
                    .map(str::to_string);
                assert!(data["events"].as_array().is_some());
            } else {
                assert!(frame.starts_with("event: error"), "{frame:.200}");
            }
        }
        let last_id = last_id.expect("a whole block before the cut");
        let height: i32 = last_id.split(':').nth(1).unwrap().parse().unwrap();
        assert!(height < 40, "the stream was not cut");
        assert!(cut.starts_with("event: block\ndata: "), "{cut:.200}");
        assert!(
            cut.contains(&format!("\"height\":{},", height + 1)),
            "{cut:.200}"
        );
        // the client's Last-Event-ID resumes at the block it did not finish
        let (mut stream, mut body) = open_free(
            addr,
            "/v1/stream?from_height=1",
            &format!("last-event-id: {last_id}\r\n"),
        )
        .await;
        let mut buffer = [0; 4096];
        while !body.windows(6).any(|w| w == b"data: ") {
            let n = stream.read(&mut buffer).await.unwrap();
            body.extend_from_slice(&buffer[..n]);
        }
        let start = String::from_utf8_lossy(&body).to_string();
        assert!(
            start.contains(&format!("\"height\":{},", height + 1)),
            "{start:.300}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replaying_streams_leave_connections_to_the_other_routes() {
    with_chain_pool("stream_pool", 0, 2, |pool| async move {
        add_big_blocks(&pool, 1..=10, 1000).await;
        let app = stream_app(&pool, &[]);
        let mut readers = Vec::new();
        for _ in 0..4 {
            let mut sse = Sse::open(&app, "/v1/stream?modules=brc20&from_height=1", None).await;
            readers.push(tokio::spawn(async move {
                for height in 1..=10 {
                    expect_block(&mut sse, "brc20", height, "b").await;
                }
            }));
        }
        for _ in 0..5 {
            let asked = std::time::Instant::now();
            let answer = get(&app, "/v1/brc20/ticker/events?ticker=ordi&limit=1").await;
            assert_eq!(answer.status, 200, "{}", answer.body);
            assert!(
                asked.elapsed() < Duration::from_secs(3),
                "{:?}",
                asked.elapsed()
            );
        }
        for reader in readers {
            reader.await.unwrap();
        }
    })
    .await;
}
