//! A one-time check of the current balances against the balance history.
//!
//! `brc20_current_balances` is kept block by block from
//! `brc20_historic_balances`, which is the record every balance read and
//! hash comes from. A database whose extra tables were built only partly
//! carries a current-balances table that misses rows. Once per database,
//! before indexing resumes, the indexer compares the two: the newest
//! historic row per (pkscript, tick), by block height then id as the
//! balance reads order them, against the current row, on the overall and
//! available balance, for every nonzero balance. On a difference it
//! rebuilds the extra tables with `initial_index_of_extra_tables`, which
//! also recounts every ticker's BRC-20 holders.
//!
//! The single-row `brc20_extras_verified` table records that the check
//! passed. db_init.sql creates it with its row, so a fresh database never
//! runs the check. The row is written only after the check, and after the
//! rebuild's commit, so an interrupted check runs again at the next start.

use std::{error::Error, time::Instant};

use sqlx::{Pool, Postgres, Row};

use super::Brc20Database;

/// The table.
pub const TABLE: &str = "brc20_extras_verified";

/// The table, created at start when missing, also in db_init.sql.
pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS public.brc20_extras_verified (
\tid int4 NOT NULL DEFAULT 1,
\tverified_at timestamptz NOT NULL DEFAULT now(),
\tCONSTRAINT brc20_extras_verified_pk PRIMARY KEY (id),
\tCONSTRAINT brc20_extras_verified_one_row CHECK (id = 1)
);
";

/// Records that the check passed, also in db_init.sql.
pub const MARK_SQL: &str =
    "INSERT INTO public.brc20_extras_verified (id) VALUES (1) ON CONFLICT DO NOTHING;";

/// Counts the nonzero balances the history gives that the current balances
/// lack or hold with other amounts (`missing`), and the nonzero current
/// balances the history does not give (`extra`).
const COMPARE_SQL: &str = "WITH latest AS (
    SELECT DISTINCT ON (pkscript, tick) pkscript, tick, overall_balance, available_balance
    FROM brc20_historic_balances
    ORDER BY pkscript, tick, block_height DESC, id DESC
), expected AS (
    SELECT pkscript, tick, overall_balance, available_balance FROM latest
    WHERE overall_balance <> 0 OR available_balance <> 0
), stored AS (
    SELECT pkscript, tick, overall_balance, available_balance FROM brc20_current_balances
    WHERE overall_balance <> 0 OR available_balance <> 0
)
SELECT
    (SELECT count(*) FROM (SELECT * FROM expected EXCEPT SELECT * FROM stored) m) AS missing,
    (SELECT count(*) FROM (SELECT * FROM stored EXCEPT SELECT * FROM expected) e) AS extra";

/// What the start-up check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The database carries the marker; nothing was compared.
    AlreadyVerified,
    /// The extra tables are empty, so they are not built yet; the indexer
    /// builds them whole from the indexed data once it nears the tip.
    NotBuilt,
    /// The current balances match the history.
    Consistent,
    /// The current balances differed and the extra tables were rebuilt.
    Repaired { missing: i64, extra: i64 },
}

/// Whether the database carries the marker, creating the table first when
/// it is missing.
pub async fn is_verified(pool: &Pool<Postgres>) -> Result<bool, sqlx::Error> {
    sqlx::raw_sql(SCHEMA_SQL).execute(pool).await?;
    Ok(sqlx::query("SELECT 1 FROM brc20_extras_verified")
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Counts the differences between the current balances and the history, as
/// (missing, extra).
pub async fn compare(pool: &Pool<Postgres>) -> Result<(i64, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // A full scan of both tables; SET LOCAL lifts a server-side timeout for
    // this transaction only.
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query(COMPARE_SQL).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((row.get("missing"), row.get("extra")))
}

/// Runs the check unless the database carries the marker, rebuilds the
/// extra tables on a difference, then records the marker.
pub async fn verify_once(database: &Brc20Database) -> Result<Verification, Box<dyn Error>> {
    let pool = &database.client;
    if is_verified(pool).await? {
        return Ok(Verification::AlreadyVerified);
    }
    let built = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM brc20_current_balances)
             OR EXISTS (SELECT 1 FROM brc20_unused_txes)",
    )
    .fetch_one(pool)
    .await?
    .get::<bool, _>(0);
    let verification = if !built {
        tracing::info!(
            "The extra tables are not built yet; they are built from the indexed data once indexing nears the tip"
        );
        Verification::NotBuilt
    } else {
        tracing::info!(
            "Checking brc20_current_balances against brc20_historic_balances once for this database"
        );
        let started = Instant::now();
        let (missing, extra) = compare(pool).await?;
        tracing::info!(
            "Checked the current balances in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        if missing == 0 && extra == 0 {
            Verification::Consistent
        } else {
            tracing::warn!(
                "brc20_current_balances differs from brc20_historic_balances: {missing} balance(s) missing or different, {extra} extra; rebuilding the extra tables"
            );
            let started = Instant::now();
            database.initial_index_of_extra_tables().await?;
            tracing::info!(
                "Rebuilt the extra tables in {:.1} s",
                started.elapsed().as_secs_f64()
            );
            Verification::Repaired { missing, extra }
        }
    };
    sqlx::query(MARK_SQL).execute(pool).await?;
    Ok(verification)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::brc20_database::SqlFiles;

    #[test]
    fn db_init_creates_the_table_with_its_marker() {
        let init = SqlFiles::get_sql_file("db_init.sql").unwrap();
        assert!(init.contains(SCHEMA_SQL));
        assert!(init.contains(MARK_SQL));
        let reset = SqlFiles::get_sql_file("db_reset.sql").unwrap();
        assert!(reset.contains(&format!("drop table if exists {TABLE};")));
    }
}

/// Postgres-backed tests, each on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use bitcoin::Network;
    use sqlx::{Connection, postgres::PgConnectOptions};

    use super::*;
    use crate::config::Brc20IndexerConfig;

    /// Runs `body` against a throwaway database, dropped even when `body`
    /// panics.
    async fn with_database<F, Fut>(tag: &str, body: F)
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
            eprintln!("OPI_TEST_PG is not 1; skipping");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("opi_brc20_extras_{tag}_{}_{nanos}", std::process::id());
        let admin = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("alper")
            .database("postgres");
        let mut root = sqlx::PgConnection::connect_with(&admin).await.unwrap();
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

    async fn database(name: &str) -> Brc20Database {
        let config = Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: Network::Signet,
            ..Default::default()
        };
        let mut database = Brc20Database::new(&config);
        database.init().await.unwrap();
        database
    }

    /// A balance change, as a block's events write it to
    /// brc20_historic_balances.
    async fn historic(
        database: &Brc20Database,
        event_id: i64,
        pkscript: &str,
        tick: &str,
        balance: i64,
        block_height: i32,
    ) {
        sqlx::query(
            "INSERT INTO brc20_historic_balances
                 (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id)
             VALUES ($1, 'w' || $1, $2, $3, $3, $4, $5)",
        )
        .bind(pkscript)
        .bind(tick)
        .bind(sqlx::types::BigDecimal::from(balance))
        .bind(block_height)
        .bind(event_id)
        .execute(&database.client)
        .await
        .unwrap();
    }

    /// A database as an old light client left it: a history of four
    /// balances, extra tables built block by block, no marker.
    async fn old_database(name: &str) -> Brc20Database {
        let mut database = database(name).await;
        historic(&database, 1, "a1", "ordi", 5, 10).await;
        historic(&database, 2, "b2", "ordi", 3, 10).await;
        historic(&database, 3, "c3", "pepe", 9, 10).await;
        database.index_extra_tables(10).await.unwrap();
        historic(&database, 4, "b2", "ordi", 0, 11).await;
        historic(&database, 5, "d4", "ordi", 2, 11).await;
        database.index_extra_tables(11).await.unwrap();
        sqlx::raw_sql("DROP TABLE brc20_extras_verified")
            .execute(&database.client)
            .await
            .unwrap();
        database
    }

    /// Every current balance as `pkscript tick overall available`.
    async fn current(database: &Brc20Database) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT pkscript || ' ' || tick || ' ' || overall_balance || ' ' || available_balance
             FROM brc20_current_balances
             WHERE overall_balance <> 0 OR available_balance <> 0 ORDER BY 1",
        )
        .fetch_all(&database.client)
        .await
        .unwrap()
    }

    /// Every ticker's holders as `tick holders`.
    async fn holders(database: &Brc20Database) -> Vec<String> {
        sqlx::query_scalar("SELECT tick || ' ' || holders FROM brc20_ticker_holders ORDER BY tick")
            .fetch_all(&database.client)
            .await
            .unwrap()
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    const EXPECTED: [&str; 3] = ["a1 ordi 5 5", "c3 pepe 9 9", "d4 ordi 2 2"];

    #[tokio::test]
    async fn an_incomplete_table_is_repaired_and_marked() {
        with_database("repair", |name| async move {
            let old = old_database(&name).await;
            assert_eq!(current(&old).await, lines(&EXPECTED));
            // A row lost, a row with a stale amount, and a row the history
            // does not give.
            sqlx::raw_sql(
                "DELETE FROM brc20_current_balances WHERE pkscript = 'c3';
                 UPDATE brc20_current_balances SET available_balance = 1 WHERE pkscript = 'a1';
                 INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height)
                     VALUES ('zz', 'wzz', 'ordi', 4, 4, 11);
                 UPDATE brc20_ticker_holders SET holders = 7;",
            )
            .execute(&old.client)
            .await
            .unwrap();
            assert_eq!(compare(&old.client).await.unwrap(), (2, 2));

            // A restart's database start leaves the check to the indexer's.
            let restarted = database(&name).await;
            assert_eq!(
                verify_once(&restarted).await.unwrap(),
                Verification::Repaired {
                    missing: 2,
                    extra: 2
                }
            );
            assert_eq!(current(&restarted).await, lines(&EXPECTED));
            assert_eq!(holders(&restarted).await, lines(&["ordi 2", "pepe 1"]));
            assert_eq!(compare(&restarted.client).await.unwrap(), (0, 0));
            assert!(is_verified(&restarted.client).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn a_consistent_table_is_marked_without_a_rebuild() {
        with_database("consistent", |name| async move {
            let database = old_database(&name).await;
            // A rebuild would write every row again, with new ids.
            let ids: Vec<i64> =
                sqlx::query_scalar("SELECT id FROM brc20_current_balances ORDER BY id")
                    .fetch_all(&database.client)
                    .await
                    .unwrap();
            assert_eq!(
                verify_once(&database).await.unwrap(),
                Verification::Consistent
            );
            let after: Vec<i64> =
                sqlx::query_scalar("SELECT id FROM brc20_current_balances ORDER BY id")
                    .fetch_all(&database.client)
                    .await
                    .unwrap();
            assert_eq!(after, ids);
            assert!(is_verified(&database.client).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn a_marked_database_skips_the_check() {
        with_database("marked", |name| async move {
            let database = old_database(&name).await;
            sqlx::raw_sql("DELETE FROM brc20_current_balances WHERE pkscript = 'c3'")
                .execute(&database.client)
                .await
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{SCHEMA_SQL}{MARK_SQL}")))
                .execute(&database.client)
                .await
                .unwrap();
            assert_eq!(
                verify_once(&database).await.unwrap(),
                Verification::AlreadyVerified
            );
            // The difference stays, so nothing compared or rebuilt.
            assert_eq!(compare(&database.client).await.unwrap(), (1, 0));
            assert_eq!(
                current(&database).await,
                lines(&["a1 ordi 5 5", "d4 ordi 2 2"])
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_fresh_database_has_the_marker() {
        with_database("fresh", |name| async move {
            let database = database(&name).await;
            assert!(is_verified(&database.client).await.unwrap());
            assert_eq!(
                verify_once(&database).await.unwrap(),
                Verification::AlreadyVerified
            );
        })
        .await;
    }

    #[tokio::test]
    async fn unbuilt_extra_tables_are_marked_and_left_for_their_first_build() {
        with_database("unbuilt", |name| async move {
            let database = database(&name).await;
            historic(&database, 1, "a1", "ordi", 5, 10).await;
            sqlx::raw_sql("DROP TABLE brc20_extras_verified")
                .execute(&database.client)
                .await
                .unwrap();
            assert_eq!(
                verify_once(&database).await.unwrap(),
                Verification::NotBuilt
            );
            assert_eq!(current(&database).await, Vec::<String>::new());
            assert!(is_verified(&database.client).await.unwrap());
        })
        .await;
    }
}
