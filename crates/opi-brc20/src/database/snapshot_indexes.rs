//! The indexes behind the API's holders snapshot,
//! `/v1/brc20/ticker/holders/snapshot`. The route takes each holder's latest
//! history row at or below a height with `DISTINCT ON`; each index lists a
//! ticker's rows in that order and carries the columns the route reads, so
//! Postgres reads them with an index-only scan, in order and without a sort.
//!
//! - `brc20_historic_balances_tick_pkscript_block_height_id_idx` on
//!   `(tick, pkscript, block_height DESC, id DESC)`, including `wallet`,
//!   `overall_balance` and `available_balance`.
//! - `brc20_prog_historical_balances_ticker_wallet_block_height_idx` on
//!   `(ticker, wallet, block_height DESC)`, including `amount`.
//!
//! db_init.sql creates them with the tables, as does
//! `prog_balances::TABLES_SQL` for the BRC2.0 table. On an existing database
//! a task started at init builds each missing one with `CREATE INDEX
//! CONCURRENTLY`, so the indexer keeps writing the tables during the build.
//! A build that failed or was cut off leaves an invalid index, which the next
//! start drops and builds again; one that another session is still building
//! is left to it. Once both are valid, the `(tick)` and `(ticker)` indexes,
//! which they lead with, are dropped.

use {
    sqlx::{Pool, Postgres, Row},
    std::time::Instant,
};

/// The indexes by name with their definitions.
pub const INDEXES: [(&str, &str); 2] = [
    (
        "brc20_historic_balances_tick_pkscript_block_height_id_idx",
        "ON public.brc20_historic_balances USING btree (tick, pkscript, block_height DESC, id DESC) INCLUDE (wallet, overall_balance, available_balance)",
    ),
    (
        "brc20_prog_historical_balances_ticker_wallet_block_height_idx",
        "ON public.brc20_prog_historical_balances USING btree (ticker, wallet, block_height DESC) INCLUDE (amount)",
    ),
];

/// The `(tick)` and `(ticker)` indexes, dropped once the indexes exist.
pub const SUPERSEDED: [&str; 2] = [
    "brc20_historic_balances_tick_idx",
    "brc20_prog_historical_balances_ticker_idx",
];

/// What the catalog holds for an index.
enum State {
    Missing,
    Valid,
    /// Invalid, with a build of it running in another session.
    Building,
    /// Invalid and left by a build that ended.
    Invalid,
}

async fn state(pool: &Pool<Postgres>, name: &str) -> Result<State, sqlx::Error> {
    let row = sqlx::query(
        "SELECT i.indisvalid,
                EXISTS (SELECT 1 FROM pg_stat_progress_create_index p WHERE p.index_relid = c.oid)
         FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid
         WHERE c.relnamespace = 'public'::regnamespace AND c.relname = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => State::Missing,
        Some(row) if row.get::<bool, _>(0) => State::Valid,
        Some(row) if row.get::<bool, _>(1) => State::Building,
        Some(_) => State::Invalid,
    })
}

/// Builds each missing index, then drops the superseded ones. A start that
/// finds every index valid only drops what is left of those.
pub async fn create_if_missing(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    for (name, definition) in INDEXES {
        match state(pool, name).await? {
            State::Valid => continue,
            State::Building => {
                tracing::info!("Index {name} is being built by another session");
                return Ok(());
            }
            State::Invalid => {
                tracing::warn!("Dropping invalid index {name} to build it again");
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP INDEX CONCURRENTLY IF EXISTS public.{name}"
                )))
                .execute(pool)
                .await?;
            }
            State::Missing => {}
        }
        tracing::info!("Building index {name}; the indexer keeps running meanwhile");
        let started = Instant::now();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} {definition}"
        )))
        .execute(pool)
        .await?;
        tracing::info!(
            "Built index {name} in {} seconds",
            started.elapsed().as_secs()
        );
    }
    for name in SUPERSEDED {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP INDEX CONCURRENTLY IF EXISTS public.{name}"
        )))
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_init_creates_the_indexes_and_not_the_superseded_ones() {
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        for (name, definition) in INDEXES {
            let sql = format!("CREATE INDEX IF NOT EXISTS {name} {definition};");
            assert!(init.contains(&sql), "{name}");
            if definition.contains("brc20_prog_") {
                assert!(
                    super::super::prog_balances::TABLES_SQL.contains(&sql),
                    "{name}"
                );
            }
        }
        for name in SUPERSEDED {
            assert!(!init.contains(name), "{name}");
            assert!(
                !super::super::prog_balances::TABLES_SQL.contains(name),
                "{name}"
            );
        }
    }
}

/// Postgres-backed test on a throwaway database. It skips unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use {
        super::*,
        sqlx::{Connection, postgres::PgConnectOptions},
    };

    async fn indexes(pool: &Pool<Postgres>) -> Vec<(String, bool)> {
        sqlx::query_as(
            "SELECT c.relname::text, i.indisvalid
             FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid
             WHERE c.relname IN (
                 'brc20_historic_balances_tick_pkscript_block_height_id_idx',
                 'brc20_prog_historical_balances_ticker_wallet_block_height_idx',
                 'brc20_historic_balances_tick_idx',
                 'brc20_prog_historical_balances_ticker_idx')
             ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_start_builds_the_missing_indexes_and_drops_the_superseded_ones() {
        if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
            eprintln!("OPI_TEST_PG is not 1; skipping");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("opi_brc20_snapshot_idx_{}_{nanos}", std::process::id());
        let admin = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username(&std::env::var("USER").unwrap())
            .database("postgres");
        let mut root = sqlx::PgConnection::connect_with(&admin).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut root)
            .await
            .unwrap();
        let pool = Pool::<Postgres>::connect_lazy_with(admin.clone().database(&name));
        let result = tokio::spawn({
            let pool = pool.clone();
            async move {
                let built = vec![
                    (INDEXES[0].0.to_string(), true),
                    (INDEXES[1].0.to_string(), true),
                ];
                sqlx::raw_sql(sqlx::AssertSqlSafe(
                    crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql")
                        .unwrap(),
                ))
                .execute(&pool)
                .await
                .unwrap();
                assert_eq!(indexes(&pool).await, built);

                // A database from before the indexes, with an invalid one
                // left by a cut-off build, has them and not the superseded
                // ones after a start.
                sqlx::raw_sql(
                    "DROP INDEX brc20_historic_balances_tick_pkscript_block_height_id_idx;
                     DROP INDEX brc20_prog_historical_balances_ticker_wallet_block_height_idx;
                     CREATE INDEX brc20_historic_balances_tick_idx ON public.brc20_historic_balances USING btree (tick);
                     CREATE INDEX brc20_prog_historical_balances_ticker_idx ON public.brc20_prog_historical_balances USING btree (ticker);
                     INSERT INTO brc20_prog_historical_balances (wallet, ticker, amount, block_height)
                     VALUES ('0xa', 't', 1, 1), ('0xa', 't', 2, 2);",
                )
                .execute(&pool)
                .await
                .unwrap();
                // A unique index the rows break fails its build and stays
                // invalid under the name.
                let failed = sqlx::query(
                    "CREATE UNIQUE INDEX CONCURRENTLY brc20_prog_historical_balances_ticker_wallet_block_height_idx
                     ON public.brc20_prog_historical_balances (ticker)",
                )
                .execute(&pool)
                .await;
                assert!(failed.is_err());
                assert_eq!(
                    indexes(&pool).await,
                    vec![
                        ("brc20_historic_balances_tick_idx".to_string(), true),
                        ("brc20_prog_historical_balances_ticker_idx".to_string(), true),
                        (INDEXES[1].0.to_string(), false),
                    ]
                );
                create_if_missing(&pool).await.unwrap();
                assert_eq!(indexes(&pool).await, built);
                // and a start that finds them changes nothing
                create_if_missing(&pool).await.unwrap();
                assert_eq!(indexes(&pool).await, built);
            }
        })
        .await;
        pool.close().await;
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
}
