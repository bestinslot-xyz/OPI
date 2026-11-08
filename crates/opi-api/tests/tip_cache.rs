//! The whole-table hash routes, `/v1/sns/get_hash_of_all_registered_names`
//! and `/v1/brc20/get_hash_of_all_current_balances`, computed once per
//! indexed block.
//!
//! The tests build a throwaway database from the module's db_init.sql on the
//! local Postgres and change the table behind a route without moving the
//! tip, so an answer that stays the same is the cached one. They skip unless
//! `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{get, with_seeded_pool},
    opi_api::config::Module,
    sqlx::{Connection, PgPool},
    std::time::Duration,
};

const NAMES: &str = "/v1/sns/get_hash_of_all_registered_names";
const BALANCES: &str = "/v1/brc20/get_hash_of_all_current_balances";

const SNS_FIXTURE: &str = r#"
INSERT INTO sns_block_hashes (block_height, block_hash) VALUES (100, 'h100'), (101, 'h101');
INSERT INTO sns_names (inscription_id, inscription_number, "name", domain, block_height)
VALUES ('n1', 1, 'a.sats', 'sats', 100), ('n2', 2, 'b.sats', 'sats', 101);
"#;

/// The names answer for the table as it is, built by Postgres.
async fn names_answer(pool: &PgPool, height: i32) -> String {
    let hash: String = sqlx::query_scalar(
        r#"select encode(sha256(convert_to(coalesce(string_agg(
                "name" || ';' || domain || ';' || inscription_id || ';' || inscription_number,
                '|' order by inscription_number), ''), 'UTF8')), 'hex')
           from sns_names"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    format!(
        r#"{{"error":null,"result":{{"current_sns_names_hash":"{hash}","indexer_version":"opi-sns-names-full-node v3.0.0","block_height":{height}}}}}"#
    )
}

async fn body(app: &Router, uri: &str) -> String {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    answer.body
}

async fn execute(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_names_hash_is_served_until_the_tip_moves() {
    with_seeded_pool(
        Module::Sns,
        "tip_cache_names",
        None,
        &[include_str!("../../opi-sns/src/db_init.sql"), SNS_FIXTURE],
        4,
        |app, pool| async move {
            let at_101 = names_answer(&pool, 101).await;
            assert_eq!(body(&app, NAMES).await, at_101);

            // A change at the same tip is not seen: the answer is cached.
            execute(
                &pool,
                "UPDATE sns_names SET domain = 'x' WHERE inscription_id = 'n1'",
            )
            .await;
            assert_eq!(body(&app, NAMES).await, at_101);

            // A new block computes anew.
            execute(
                &pool,
                "INSERT INTO sns_block_hashes (block_height, block_hash) VALUES (102, 'h102')",
            )
            .await;
            let at_102 = names_answer(&pool, 102).await;
            assert_ne!(at_102, at_101);
            assert_eq!(body(&app, NAMES).await, at_102);

            // So does another block at the same height, after a reorg.
            execute(
                &pool,
                "DELETE FROM sns_block_hashes WHERE block_height = 102;
                 INSERT INTO sns_block_hashes (block_height, block_hash) VALUES (102, 'h102b');
                 UPDATE sns_names SET domain = 'y' WHERE inscription_id = 'n2';",
            )
            .await;
            let at_102b = names_answer(&pool, 102).await;
            assert_ne!(at_102b, at_102);
            assert_eq!(body(&app, NAMES).await, at_102b);
        },
    )
    .await;
}

#[tokio::test]
async fn concurrent_requests_share_one_computation() {
    with_seeded_pool(
        Module::Sns,
        "tip_cache_flight",
        None,
        &[include_str!("../../opi-sns/src/db_init.sql"), SNS_FIXTURE],
        8,
        |app, pool| async move {
            let expected = names_answer(&pool, 101).await;
            // The table is locked, so a computation waits on its read.
            let mut locker = pool.acquire().await.unwrap().detach();
            let mut lock = locker.begin().await.unwrap();
            sqlx::query("LOCK TABLE sns_names IN ACCESS EXCLUSIVE MODE")
                .execute(&mut *lock)
                .await
                .unwrap();
            let requests: Vec<_> = (0..5)
                .map(|_| {
                    let app = app.clone();
                    tokio::spawn(async move { body(&app, NAMES).await })
                })
                .collect();
            tokio::time::sleep(Duration::from_millis(500)).await;
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_locks
                 where relation = 'sns_names'::regclass and not granted",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(waiting, 1, "one computation reads the table");
            lock.rollback().await.unwrap();
            for request in requests {
                assert_eq!(request.await.unwrap(), expected);
            }
        },
    )
    .await;
}

const BRC20_FIXTURE: &str = r#"
INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (100, 'h100');
INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height)
VALUES ('0014aa', 'tb1qa', 'ordi', 10, 5, 100), ('0014bb', 'tb1qb', 'ordi', 0, 0, 100);
"#;

/// The balances answer for the table as it is, built by Postgres.
async fn balances_answer(pool: &PgPool, height: i32) -> String {
    let hash: String = sqlx::query_scalar(
        r#"select encode(sha256(convert_to(coalesce(string_agg(
                pkscript || ';' || tick || ';' || overall_balance || ';' || available_balance,
                '|' order by pkscript collate "C", tick collate "C"), ''), 'UTF8')), 'hex')
           from brc20_current_balances where overall_balance <> 0"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    format!(
        r#"{{"error":null,"result":{{"current_balances_hash":"{hash}","indexer_version":"opi-brc20-rs-node v2.0.0","block_height":{height}}}}}"#
    )
}

#[tokio::test]
async fn balances_written_ahead_of_the_tip_are_not_cached() {
    with_seeded_pool(
        Module::Brc20,
        "tip_cache_balances",
        None,
        &[
            include_str!("../../opi-brc20/src/database/sql/db_init.sql"),
            BRC20_FIXTURE,
        ],
        4,
        |app, pool| async move {
            let at_100 = balances_answer(&pool, 100).await;

            // Block 101's balances commit before its block hash: an answer
            // that shows them is computed again on every request.
            let ahead = |amount: u32| {
                format!(
                    "UPDATE brc20_current_balances SET overall_balance = {amount}, block_height = 101 WHERE pkscript = '0014bb'"
                )
            };
            for amount in [12, 13] {
                execute(&pool, &ahead(amount)).await;
                let answer = balances_answer(&pool, 100).await;
                assert_ne!(answer, at_100);
                assert_eq!(body(&app, BALANCES).await, answer);
            }
            // A reorg drops block 101 before its hash lands: the answer at
            // the tip is the state at the tip again.
            execute(
                &pool,
                "UPDATE brc20_current_balances SET overall_balance = 0, block_height = 100 WHERE pkscript = '0014bb'",
            )
            .await;
            assert_eq!(body(&app, BALANCES).await, at_100);
            // and it is the cached one: a change at the same tip is not seen
            execute(&pool, "UPDATE brc20_current_balances SET overall_balance = 11 WHERE pkscript = '0014aa'").await;
            assert_eq!(body(&app, BALANCES).await, at_100);

            execute(&pool, "INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (101, 'h101')").await;
            let at_101 = balances_answer(&pool, 101).await;
            assert_ne!(at_101, at_100);
            assert_eq!(body(&app, BALANCES).await, at_101);
        },
    )
    .await;
}
