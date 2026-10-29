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

/// The rows of block `height` on the branch `tag` of every module: its
/// hash `<tag><height>`, and `events` brc20 mints of ordi, one bitmap and
/// one sns name.
fn block(height: i32, tag: &str, events: i32) -> String {
    let mut sql = format!(
        "INSERT INTO brc20_block_hashes (block_height, block_hash, block_time)
             VALUES ({height}, '{tag}{height}', {});
         INSERT INTO bitmap_block_hashes (block_height, block_hash) VALUES ({height}, '{tag}{height}');
         INSERT INTO sns_block_hashes (block_height, block_hash) VALUES ({height}, '{tag}{height}');
         INSERT INTO bitmaps (id, inscription_id, inscription_number, bitmap_number, block_height)
             VALUES ({height}, '{tag}bm{height}i0', {height}, {height}, {height});
         INSERT INTO sns_names (id, inscription_id, inscription_number, \"name\", domain, block_height)
             VALUES ({height}, '{tag}sn{height}i0', {height}, '{tag}{height}.sats', 'sats', {height});",
        1000 + height
    );
    for i in 0..events {
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

/// The events of a brc20 block at `height`.
fn events_of(height: i32) -> i32 {
    if height == 3 { 250 } else { 2 }
}

async fn add_blocks(pool: &PgPool, heights: std::ops::RangeInclusive<i32>, tag: &str) {
    for height in heights {
        // the indexer commits each block with its hash row
        let sql = format!("BEGIN; {} COMMIT;", block(height, tag, events_of(height)));
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// A reorg as the indexer runs it: every module's rows above `kept` go in
/// one transaction, then the blocks of branch `tag` up to `to` come.
async fn reorg(pool: &PgPool, kept: i32, to: i32, tag: &str) {
    let sql = format!(
        "BEGIN;
         DELETE FROM brc20_events WHERE block_height > {kept};
         DELETE FROM brc20_block_hashes WHERE block_height > {kept};
         DELETE FROM bitmaps WHERE block_height > {kept};
         DELETE FROM bitmap_block_hashes WHERE block_height > {kept};
         DELETE FROM sns_names WHERE block_height > {kept};
         DELETE FROM sns_block_hashes WHERE block_height > {kept};
         COMMIT;"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await
        .unwrap();
    add_blocks(pool, kept + 1..=to, tag).await;
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
        10,
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
    assert_eq!(
        frame.id.as_deref(),
        Some(format!("{module}:{height}:{hash}").as_str()),
        "{frame:?}"
    );
    assert_eq!(frame.data["module"], module);
    assert_eq!(frame.data["height"], height);
    assert_eq!(frame.data["hash"], hash);
    frame
}

async fn expect_rollback(sse: &mut Sse, module: &str, height: i32) {
    let frame = message(sse).await;
    assert_eq!(frame.event, "rollback", "{frame:?}");
    assert_eq!(frame.data, json!({"module": module, "height": height}));
    assert_eq!(frame.id, None);
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
        expect_rollback(&mut sse, "brc20", 7).await;
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
        expect_rollback(&mut watching, "brc20", 5).await;
        let mut sse = Sse::open(&app, "/v1/stream?modules=brc20", Some("brc20:6:b6")).await;
        expect_rollback(&mut sse, "brc20", 5).await;
        for height in 6..=10 {
            expect_block(&mut sse, "brc20", height, "x").await;
        }
        drop((sse, watching));

        // a stale id this process saw no reorg of rolls back as far as a
        // reorg may go
        let fresh = stream_app(&pool, &[]);
        let mut sse = Sse::open(&fresh, "/v1/stream?modules=brc20", Some("brc20:6:b6")).await;
        expect_rollback(&mut sse, "brc20", 0).await;
        expect_block(&mut sse, "brc20", 1, "b").await;
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
                    let uri = format!("/v1/{module}/by_inscription?inscription_id=b{short}{height}i0");
                    let answer: Value = serde_json::from_str(&get(&app, &uri).await.body).unwrap();
                    assert_eq!(events[0], answer["result"]);
                }
            }
        }
        // a resume within a height goes on with the modules after it
        drop(sse);
        let mut sse = Sse::open(&app, "/v1/stream?modules=sns,bitmap,brc20", Some("brc20:2:b2")).await;
        expect_block(&mut sse, "sns", 2, "b").await;
        expect_block(&mut sse, "bitmap", 3, "b").await;
        // a reorg of one module rolls that module back alone
        drop(sse);
        let mut sse = Sse::open(&app, "/v1/stream?modules=sns,bitmap", None).await;
        sqlx::raw_sql(
            "BEGIN; DELETE FROM sns_names WHERE block_height > 1;
             DELETE FROM sns_block_hashes WHERE block_height > 1; COMMIT;",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut sns = String::new();
        for height in 2..=3 {
            sns.push_str(&format!(
                "INSERT INTO sns_block_hashes (block_height, block_hash) VALUES ({height}, 'x{height}');"
            ));
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(sns)).execute(&pool).await.unwrap();
        // the stream started after the tip, so nothing was delivered: the
        // replaced blocks need no rollback
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let answer = get(&app, "/v1/stream").await;
            if answer.status == 200 {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "{}", answer.body);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_slow_subscriber_is_cut_with_an_error_event() {
    with_chain("stream_slow", 6, |pool| async move {
        let app = stream_app(&pool, &[("STREAM_BUFFER", "1"), ("STREAM_HEARTBEAT_SECS", "1")]);
        let mut sse = Sse::open(&app, "/v1/stream?from_height=1", None).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
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
