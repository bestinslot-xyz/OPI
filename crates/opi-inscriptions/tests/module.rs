use std::future::Future;

use opi_core::{BlockView, InscriptionFeed, InscriptionGenesis, InscriptionMove, Module};
use opi_inscriptions::{InscriptionsConfig, InscriptionsModule, address};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

fn admin() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(5432)
        .username("alper")
        .database("postgres")
}

fn config(database: &str) -> InscriptionsConfig {
    InscriptionsConfig {
        db_user: "alper".into(),
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_database: database.into(),
        db_password: None,
        db_ssl: false,
        network_type: "regtest".into(),
    }
}

async fn connect(database: &str) -> PgConnection {
    PgConnection::connect_with(
        &PgConnectOptions::new()
            .host("127.0.0.1")
            .username("alper")
            .database(database),
    )
    .await
    .unwrap()
}

/// Runs `body` against a fresh database that is dropped afterwards, on
/// failure too.
async fn with_database<F, Fut>(prefix: &str, body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("opi_inscriptions_{prefix}_{}_{nanos}", std::process::id());
    let mut root = PgConnection::connect_with(&admin()).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut root)
        .await
        .unwrap();
    let result = tokio::spawn(body(name.clone())).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&mut root)
    .await
    .unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

fn txid(byte: &str) -> String {
    byte.repeat(32)
}

fn id(byte: &str) -> String {
    format!("{}i0", txid(byte))
}

fn satpoint(byte: &str, vout: u32, offset: u64) -> String {
    format!("{}:{vout}:{offset}", txid(byte))
}

fn pk1() -> Vec<u8> {
    hex::decode(format!("0014{}", "11".repeat(20))).unwrap()
}

fn pk2() -> Vec<u8> {
    hex::decode(format!("5120{}", "22".repeat(32))).unwrap()
}

fn op_return() -> Vec<u8> {
    vec![0x6a]
}

fn create(byte: &str, seq: u32, number: i32, content_type: &[u8], at: String) -> InscriptionMove {
    InscriptionMove {
        inscription_id: id(byte),
        sequence_number: seq,
        genesis: Some(InscriptionGenesis {
            number,
            charms: 0,
            content_type: Some(content_type.to_vec()),
        }),
        satpoint: at,
        pkscript: pk1(),
    }
}

fn transfer(byte: &str, seq: u32, to: String, pkscript: Vec<u8>) -> InscriptionMove {
    InscriptionMove {
        inscription_id: id(byte),
        sequence_number: seq,
        genesis: None,
        satpoint: to,
        pkscript,
    }
}

fn view(height: u32, moves: Vec<InscriptionMove>) -> BlockView {
    BlockView {
        height,
        hash: format!("hash{height}"),
        time: 1_700_000_000 + height,
        inscriptions: InscriptionFeed {
            records: Vec::new(),
            infos: Default::default(),
        },
        runes: Default::default(),
        inscription_moves: moves,
    }
}

/// (inscription_id, outpoint, offset, address, last_moved_height) of every
/// entry, by sequence number.
async fn entries(conn: &mut PgConnection) -> Vec<(String, String, i64, Option<String>, i32)> {
    sqlx::query(
        "SELECT inscription_id, outpoint, \"offset\", address, last_moved_height \
         FROM inscriptions_entries ORDER BY sequence_number",
    )
    .fetch_all(conn)
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)))
    .collect()
}

fn addr(pkscript: &[u8]) -> Option<String> {
    address(pkscript, "regtest")
}

/// Height 1 creates A and B; height 2 moves A twice and burns B; height 3
/// creates C and moves A again.
async fn three_blocks(module: &mut InscriptionsModule) {
    module
        .apply(&view(
            1,
            vec![
                create(
                    "aa",
                    0,
                    0,
                    b"text/plain;charset=utf-8",
                    satpoint("aa", 0, 0),
                ),
                create("bb", 1, -1, b"Image/PNG", satpoint("bb", 0, 0)),
            ],
        ))
        .await
        .unwrap();
    module
        .apply(&view(
            2,
            vec![
                transfer("aa", 0, satpoint("cc", 1, 5), pk2()),
                transfer("aa", 0, satpoint("dd", 0, 0), pk2()),
                transfer("bb", 1, satpoint("ee", 0, 0), op_return()),
            ],
        ))
        .await
        .unwrap();
    module
        .apply(&view(
            3,
            vec![
                create("ff", 2, 1, b"application/json", satpoint("ff", 0, 0)),
                transfer("aa", 0, satpoint("gg", 0, 330), pk1()),
            ],
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn moves_set_the_current_location_and_owner() {
    with_database("apply", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        three_blocks(&mut module).await;
        let mut conn = connect(&name).await;
        assert_eq!(
            entries(&mut conn).await,
            vec![
                (id("aa"), format!("{}:0", txid("gg")), 330, addr(&pk1()), 3),
                (id("bb"), format!("{}:0", txid("ee")), 0, None, 2),
                (id("ff"), format!("{}:0", txid("ff")), 0, addr(&pk1()), 3),
            ]
        );
        let types: Vec<(Option<String>, Option<String>, i64, i32)> = sqlx::query_as(
            "SELECT content_type, media_type, \"number\", genesis_height \
             FROM inscriptions_entries ORDER BY sequence_number",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            types,
            vec![
                (
                    Some("text/plain;charset=utf-8".into()),
                    Some("text/plain".into()),
                    0,
                    1
                ),
                (Some("Image/PNG".into()), Some("image/png".into()), -1, 1),
                (
                    Some("application/json".into()),
                    Some("application/json".into()),
                    1,
                    3
                ),
            ]
        );
        let events: Vec<(i64, i32, Option<String>, String)> = sqlx::query_as(
            "SELECT id, block_height, old_outpoint, new_outpoint FROM inscriptions_events \
             ORDER BY id",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let ids: Vec<i64> = events.iter().map(|e| e.0).collect();
        assert_eq!(
            ids,
            vec![
                1 << 32,
                (1 << 32) + 1,
                2 << 32,
                (2 << 32) + 1,
                (2 << 32) + 2,
                3 << 32,
                (3 << 32) + 1
            ]
        );
        // The second move of A in block 2 starts where the first left it.
        assert_eq!(events[3].2, Some(format!("{}:1", txid("cc"))));
        assert_eq!(events[0].2, None);
    })
    .await;
}

#[tokio::test]
async fn rollback_restores_the_prior_locations() {
    with_database("rollback", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        three_blocks(&mut module).await;
        let mut conn = connect(&name).await;

        module.rollback_to(2).await.unwrap();
        assert_eq!(
            entries(&mut conn).await,
            vec![
                (id("aa"), format!("{}:0", txid("dd")), 0, addr(&pk2()), 2),
                (id("bb"), format!("{}:0", txid("ee")), 0, None, 2),
            ]
        );

        module.rollback_to(1).await.unwrap();
        assert_eq!(
            entries(&mut conn).await,
            vec![
                (id("aa"), format!("{}:0", txid("aa")), 0, addr(&pk1()), 1),
                (id("bb"), format!("{}:0", txid("bb")), 0, addr(&pk1()), 1),
            ]
        );
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 1);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM inscriptions_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(events, 2);

        // The replaced blocks apply again on top.
        module
            .apply(&view(
                2,
                vec![transfer("bb", 1, satpoint("hh", 0, 0), pk2())],
            ))
            .await
            .unwrap();
        assert_eq!(
            entries(&mut conn).await,
            vec![
                (id("aa"), format!("{}:0", txid("aa")), 0, addr(&pk1()), 1),
                (id("bb"), format!("{}:0", txid("hh")), 0, addr(&pk2()), 2),
            ]
        );
    })
    .await;
}

#[tokio::test]
async fn a_transfer_of_an_unknown_inscription_fails_the_block() {
    with_database("unknown", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        let block = view(
            1,
            vec![
                create("aa", 0, 0, b"text/plain", satpoint("aa", 0, 0)),
                transfer("bb", 1, satpoint("cc", 0, 0), pk2()),
            ],
        );
        let error = module.apply(&block).await.unwrap_err();
        assert!(format!("{error:#}").contains("not stored"), "{error:#}");
        let mut conn = connect(&name).await;
        assert!(entries(&mut conn).await.is_empty());
        assert!(module.cursor().await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn a_creation_moved_in_its_own_block_is_stored_at_its_last_location() {
    with_database("same_block", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        module
            .apply(&view(
                1,
                vec![
                    create("aa", 0, 0, b"text/plain", satpoint("aa", 0, 0)),
                    transfer("aa", 0, satpoint("bb", 2, 7), pk2()),
                ],
            ))
            .await
            .unwrap();
        let mut conn = connect(&name).await;
        assert_eq!(
            entries(&mut conn).await,
            vec![(id("aa"), format!("{}:2", txid("bb")), 7, addr(&pk2()), 1)]
        );
        module.rollback_to(0).await.unwrap();
        assert!(entries(&mut conn).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn prune_drops_old_events_and_block_hashes() {
    with_database("prune", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        three_blocks(&mut module).await;
        module.prune_below(3).await.unwrap();
        let mut conn = connect(&name).await;
        let heights: Vec<i32> =
            sqlx::query_scalar("SELECT DISTINCT block_height FROM inscriptions_events")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(heights, vec![3]);
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 3);
        assert_eq!(module.block_hash(2).await.unwrap(), None);
        // A rollback within the kept window still restores block 3's moves.
        module.rollback_to(2).await.unwrap();
        assert_eq!(
            entries(&mut conn).await,
            vec![
                (id("aa"), format!("{}:0", txid("dd")), 0, addr(&pk2()), 2),
                (id("bb"), format!("{}:0", txid("ee")), 0, None, 2),
            ]
        );
    })
    .await;
}

#[tokio::test]
async fn a_restart_drops_rows_above_the_cursor_and_refuses_another_network() {
    with_database("restart", |name| async move {
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        three_blocks(&mut module).await;
        let mut conn = connect(&name).await;
        sqlx::query("DELETE FROM inscriptions_block_hashes WHERE block_height = 3")
            .execute(&mut conn)
            .await
            .unwrap();
        let mut module = InscriptionsModule::new(config(&name)).await.unwrap();
        assert_eq!(module.cursor().await.unwrap().unwrap().height, 2);
        assert_eq!(entries(&mut conn).await.len(), 2);

        let mut other = config(&name);
        other.network_type = "signet".into();
        let error = InscriptionsModule::new(other).await.err().unwrap();
        assert!(
            format!("{error:#}").contains("holds network regtest"),
            "{error:#}"
        );
    })
    .await;
}
