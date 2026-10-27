//! Holder counts per ticker, for the API's ticker routes.
//!
//! `brc20_ticker_holders` holds, per ticker, `holders`: the pkScripts with a
//! positive BRC-20 balance in `brc20_current_balances` other than the BRC2.0
//! module's pkScript (`BRC20_PROG_OP_RETURN_PKSCRIPT`, whose balance the
//! BRC2.0 balances list per EVM address), and `prog_holders`: the EVM
//! wallets with a positive BRC2.0 balance in
//! `brc20_prog_current_balances`. The two counts are kept apart: a Bitcoin
//! address holding both kinds counts once in each, and a merged count of
//! unique Bitcoin addresses would need a join over every holder of the ticker
//! in every block.
//!
//! Each count is recomputed from the current balances for the tickers whose
//! balances a write changes, in the transaction of that write: a block's
//! BRC-20 balances (`index_extra_tables`), its BRC2.0 balances
//! (`prog_balances::write_block`, which also serves the BRC2.0 fill), the
//! rebuild of the BRC-20 balances, and both reorg paths. A recount reads the
//! ticker's positive balances through a partial index, so a block's cost is
//! one index-only count per ticker it touches. Nothing here feeds an event,
//! hash or report.
//!
//! The table and the API's indexes are created at start when missing, and a
//! new table is filled from the current balances in the transaction that
//! creates it. The SQL names the module's pkScript as a literal, so the
//! partial index's predicate matches every count's.

use std::collections::BTreeSet;

use sqlx::{PgConnection, Pool, Postgres, Row};

/// The table.
pub const TABLE: &str = "brc20_ticker_holders";

/// The table and the indexes the API reads with, also in db_init.sql.
pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS public.brc20_ticker_holders (
	tick text NOT NULL,
	holders int8 NOT NULL DEFAULT 0,
	prog_holders int8 NOT NULL DEFAULT 0,
	CONSTRAINT brc20_ticker_holders_pk PRIMARY KEY (tick)
);
CREATE INDEX IF NOT EXISTS brc20_current_balances_tick_holder_idx ON public.brc20_current_balances USING btree (tick, overall_balance DESC) WHERE overall_balance > 0 AND pkscript <> '6a09425243323050524f47';
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_ticker_positive_idx ON public.brc20_prog_current_balances USING btree (ticker, amount DESC) WHERE amount > 0;
CREATE INDEX IF NOT EXISTS brc20_tickers_tick_pattern_idx ON public.brc20_tickers USING btree (tick text_pattern_ops);
";

/// The API's indexes by name, each created at start when missing.
pub const INDEXES: [(&str, &str); 3] = [
    (
        "brc20_current_balances_tick_holder_idx",
        "CREATE INDEX IF NOT EXISTS brc20_current_balances_tick_holder_idx ON public.brc20_current_balances USING btree (tick, overall_balance DESC) WHERE overall_balance > 0 AND pkscript <> '6a09425243323050524f47'",
    ),
    (
        "brc20_prog_current_balances_ticker_positive_idx",
        "CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_ticker_positive_idx ON public.brc20_prog_current_balances USING btree (ticker, amount DESC) WHERE amount > 0",
    ),
    (
        "brc20_tickers_tick_pattern_idx",
        "CREATE INDEX IF NOT EXISTS brc20_tickers_tick_pattern_idx ON public.brc20_tickers USING btree (tick text_pattern_ops)",
    ),
];

/// Creates each missing API index, then the table when it is missing, filled
/// from the current balances in the transaction that creates it. An existing
/// table whose BRC2.0 tables were just created (`prog_tables_created`, empty
/// and waiting for their fill) has its BRC2.0 counts zeroed; the fill counts
/// them again as it writes. Returns whether it created the table.
pub async fn create_if_missing(
    pool: &Pool<Postgres>,
    prog_tables_created: bool,
) -> Result<bool, sqlx::Error> {
    for (name, sql) in INDEXES {
        // The lookup comes first, so a start that finds the index never
        // waits on the table lock.
        let exists =
            sqlx::query("SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1")
                .bind(name)
                .fetch_optional(pool)
                .await?
                .is_some();
        if !exists {
            tracing::info!("Creating index {name}");
            sqlx::query(sql).execute(pool).await?;
        }
    }

    let missing: bool = sqlx::query("SELECT to_regclass('brc20_ticker_holders') IS NULL")
        .fetch_one(pool)
        .await?
        .get(0);
    if !missing {
        if prog_tables_created {
            sqlx::query!(
                "UPDATE brc20_ticker_holders SET prog_holders = 0 WHERE prog_holders <> 0"
            )
            .execute(pool)
            .await?;
        }
        // A table counted with the module's pkScript among the holders is
        // recounted for the tickers that pkScript holds; with nothing to
        // correct this changes no row.
        sqlx::query!(
            "INSERT INTO brc20_ticker_holders (tick, holders)
             SELECT m.tick,
                    (SELECT count(*) FROM brc20_current_balances b
                     WHERE b.tick = m.tick AND b.overall_balance > 0
                       AND b.pkscript <> '6a09425243323050524f47')
             FROM brc20_current_balances m
             WHERE m.pkscript = '6a09425243323050524f47' AND m.overall_balance > 0
             ON CONFLICT (tick) DO UPDATE SET holders = excluded.holders"
        )
        .execute(pool)
        .await?;
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(SCHEMA_SQL).execute(&mut *tx).await?;
    sqlx::query!(
        "INSERT INTO brc20_ticker_holders (tick, holders, prog_holders)
         SELECT s.tick, sum(s.holders)::int8, sum(s.prog_holders)::int8
         FROM (
             SELECT tick, count(*) AS holders, 0::int8 AS prog_holders
             FROM brc20_current_balances
             WHERE overall_balance > 0 AND pkscript <> '6a09425243323050524f47'
             GROUP BY tick
             UNION ALL
             SELECT ticker, 0::int8, count(*)
             FROM brc20_prog_current_balances WHERE amount > 0 GROUP BY ticker
         ) s
         GROUP BY s.tick"
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Each ticker once, so one statement updates each row once.
fn distinct<'a>(ticks: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    ticks
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Recounts the BRC-20 holders of `ticks` from `brc20_current_balances`.
pub async fn recount_brc20<'a>(
    conn: &mut PgConnection,
    ticks: impl IntoIterator<Item = &'a str>,
) -> Result<(), sqlx::Error> {
    let ticks = distinct(ticks);
    if ticks.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO brc20_ticker_holders (tick, holders)
         SELECT t.tick,
                (SELECT count(*) FROM brc20_current_balances b
                 WHERE b.tick = t.tick AND b.overall_balance > 0
                   AND b.pkscript <> '6a09425243323050524f47')
         FROM UNNEST($1::text[]) AS t(tick)
         ON CONFLICT (tick) DO UPDATE SET holders = excluded.holders",
        &ticks
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Recounts the BRC2.0 holders of `tickers` from
/// `brc20_prog_current_balances`.
pub async fn recount_prog<'a>(
    conn: &mut PgConnection,
    tickers: impl IntoIterator<Item = &'a str>,
) -> Result<(), sqlx::Error> {
    let tickers = distinct(tickers);
    if tickers.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO brc20_ticker_holders (tick, prog_holders)
         SELECT t.tick,
                (SELECT count(*) FROM brc20_prog_current_balances b
                 WHERE b.ticker = t.tick AND b.amount > 0)
         FROM UNNEST($1::text[]) AS t(tick)
         ON CONFLICT (tick) DO UPDATE SET prog_holders = excluded.prog_holders",
        &tickers
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Recounts the BRC-20 holders of every ticker, after the BRC-20 current
/// balances are rebuilt.
pub async fn recount_all_brc20(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("UPDATE brc20_ticker_holders SET holders = 0 WHERE holders <> 0")
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "INSERT INTO brc20_ticker_holders (tick, holders)
         SELECT tick, count(*) FROM brc20_current_balances
         WHERE overall_balance > 0 AND pkscript <> '6a09425243323050524f47'
         GROUP BY tick
         ON CONFLICT (tick) DO UPDATE SET holders = excluded.holders"
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_init_creates_the_table_and_indexes() {
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        assert!(init.contains(SCHEMA_SQL));
        for (name, sql) in INDEXES {
            assert!(SCHEMA_SQL.contains(&format!("{sql};")), "{name}");
            assert!(sql.contains(name), "{name}");
        }
        let reset =
            crate::database::brc20_database::SqlFiles::get_sql_file("db_reset.sql").unwrap();
        assert!(reset.contains(&format!("drop table if exists {TABLE};")));
    }

    #[test]
    fn the_sql_names_the_module_pkscript() {
        let literal = format!("'{}'", crate::config::BRC20_PROG_OP_RETURN_PKSCRIPT);
        assert!(INDEXES[0].1.contains(&format!("pkscript <> {literal}")));
        let source = include_str!("ticker_holders.rs");
        // the index, the start-up correction (twice), the fill and both
        // recounts
        assert!(source.matches(&literal).count() >= 6);
    }

    #[test]
    fn recounts_name_each_ticker_once() {
        assert_eq!(
            distinct(["pepe", "ordi", "pepe", "ordi"]),
            vec!["ordi".to_string(), "pepe".to_string()]
        );
    }
}

/// Postgres-backed tests, each on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use bitcoin::Network;
    use sqlx::{Connection, postgres::PgConnectOptions};

    use super::*;
    use crate::{
        config::Brc20IndexerConfig,
        database::{
            Brc20Database,
            prog_balances::{self, BalanceEvent, TransferLog, ZERO_ADDRESS},
        },
    };

    const ALICE: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TOKEN: &str = "0x1111111111111111111111111111111111111111";
    const OTHER_TOKEN: &str = "0x2222222222222222222222222222222222222222";
    const MODULE: &str = crate::config::BRC20_PROG_OP_RETURN_PKSCRIPT;

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
        let name = format!("opi_brc20_holders_{tag}_{}_{nanos}", std::process::id());
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

    /// Every row of the table as `tick holders prog_holders`.
    async fn counts(database: &Brc20Database) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT tick || ' ' || holders || ' ' || prog_holders
             FROM brc20_ticker_holders ORDER BY tick",
        )
        .fetch_all(&database.client)
        .await
        .unwrap()
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn created(ticker: &str, contract: &str) -> BalanceEvent {
        BalanceEvent::Created {
            ticker: ticker.into(),
            ticker_hash: format!("hash-{ticker}"),
            contract_address: contract.into(),
        }
    }

    fn transfer(contract: &str, from: &str, to: &str, amount: u128) -> BalanceEvent {
        BalanceEvent::Transfer {
            contract_address: contract.into(),
            transfer: Ok(TransferLog {
                from: from.into(),
                to: to.into(),
                amount,
            }),
        }
    }

    /// A BRC-20 balance change at `block_height`, as the block's events
    /// write it to brc20_historic_balances.
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

    async fn index_block(database: &mut Brc20Database, height: i32, events: &[BalanceEvent]) {
        database.index_extra_tables(height).await.unwrap();
        database
            .set_block_hashes(height, &format!("h{height}"), "", None, events)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn blocks_and_reorgs_keep_both_counts() {
        with_database("blocks", |name| async move {
            let mut database = database(&name).await;
            // Block 10: three BRC-20 holders of ordi, one of pepe, and the
            // BRC2.0 module's pkScript, which does not count; ordi on BRC2.0
            // with two holders.
            historic(&database, 1, "a1", "ordi", 5, 10).await;
            historic(&database, 2, "b2", "ordi", 3, 10).await;
            historic(&database, 3, "c3", "ordi", 1, 10).await;
            historic(&database, 4, "c3", "pepe", 9, 10).await;
            historic(&database, 6, MODULE, "ordi", 1_000, 10).await;
            index_block(
                &mut database,
                10,
                &[
                    created("ordi", TOKEN),
                    transfer(TOKEN, ZERO_ADDRESS, ALICE, 1_000),
                    transfer(TOKEN, ALICE, BOB, 100),
                ],
            )
            .await;
            assert_eq!(counts(&database).await, lines(&["ordi 3 2", "pepe 1 0"]));

            // Block 11: a1 empties its ordi; Bob sends all his ordi back and
            // pepe starts on BRC2.0.
            historic(&database, 5, "a1", "ordi", 0, 11).await;
            index_block(
                &mut database,
                11,
                &[
                    transfer(TOKEN, BOB, ALICE, 100),
                    created("pepe", OTHER_TOKEN),
                    transfer(OTHER_TOKEN, ZERO_ADDRESS, BOB, 7),
                ],
            )
            .await;
            assert_eq!(counts(&database).await, lines(&["ordi 2 1", "pepe 1 1"]));

            // A block that changes no balance keeps the counts.
            index_block(&mut database, 12, &[]).await;
            assert_eq!(counts(&database).await, lines(&["ordi 2 1", "pepe 1 1"]));

            database.reorg(10).await.unwrap();
            assert_eq!(counts(&database).await, lines(&["ordi 3 2", "pepe 1 0"]));

            database.reorg(9).await.unwrap();
            assert_eq!(counts(&database).await, lines(&["ordi 0 0", "pepe 0 0"]));
        })
        .await;
    }

    #[tokio::test]
    async fn rebuilding_the_brc20_balances_recounts_every_ticker() {
        with_database("rebuild", |name| async move {
            let mut database = database(&name).await;
            historic(&database, 1, "a1", "ordi", 5, 10).await;
            historic(&database, 2, "b2", "ordi", 3, 10).await;
            historic(&database, 3, MODULE, "ordi", 7, 10).await;
            index_block(&mut database, 10, &[]).await;
            sqlx::raw_sql(
                "UPDATE brc20_ticker_holders SET holders = 99;
                 INSERT INTO brc20_ticker_holders (tick, holders) VALUES ('gone', 4);",
            )
            .execute(&database.client)
            .await
            .unwrap();
            database.initial_index_of_extra_tables().await.unwrap();
            assert_eq!(counts(&database).await, lines(&["gone 0 0", "ordi 2 0"]));
        })
        .await;
    }

    #[tokio::test]
    async fn a_database_without_the_table_gets_it_filled_at_start() {
        with_database("fill", |name| async move {
            let mut first = database(&name).await;
            historic(&first, 1, "a1", "ordi", 5, 10).await;
            historic(&first, 2, "b2", "ordi", 0, 10).await;
            historic(&first, 3, "b2", "pepe", 2, 10).await;
            historic(&first, 4, MODULE, "ordi", 1_000, 10).await;
            index_block(
                &mut first,
                10,
                &[
                    created("ordi", TOKEN),
                    transfer(TOKEN, ZERO_ADDRESS, ALICE, 1_000),
                ],
            )
            .await;
            let indexed = counts(&first).await;
            assert_eq!(indexed, lines(&["ordi 1 1", "pepe 1 0"]));

            sqlx::raw_sql(
                "DROP TABLE brc20_ticker_holders;
                 DROP INDEX brc20_current_balances_tick_holder_idx;
                 DROP INDEX brc20_prog_current_balances_ticker_positive_idx;
                 DROP INDEX brc20_tickers_tick_pattern_idx;",
            )
            .execute(&first.client)
            .await
            .unwrap();
            let restarted = database(&name).await;
            assert_eq!(counts(&restarted).await, indexed);
            for (index, _) in INDEXES {
                let found: Option<String> = sqlx::query_scalar(
                    "SELECT indexname::text FROM pg_indexes WHERE indexname = $1",
                )
                .bind(index)
                .fetch_optional(&restarted.client)
                .await
                .unwrap();
                assert_eq!(found.as_deref(), Some(index));
            }
            assert!(!create_if_missing(&restarted.client, false).await.unwrap());
            assert_eq!(counts(&restarted).await, indexed);

            // A table that counted the module's pkScript is corrected at
            // start.
            sqlx::raw_sql("UPDATE brc20_ticker_holders SET holders = 2 WHERE tick = 'ordi'")
                .execute(&restarted.client)
                .await
                .unwrap();
            let corrected = database(&name).await;
            assert_eq!(counts(&corrected).await, indexed);

            // BRC2.0 tables created again start their counts from zero; the
            // fill counts them as it writes.
            sqlx::raw_sql("DROP TABLE brc20_prog_current_balances")
                .execute(&restarted.client)
                .await
                .unwrap();
            let refilling = database(&name).await;
            assert_eq!(
                prog_balances::fill_progress(&refilling.client)
                    .await
                    .unwrap(),
                Some(None)
            );
            assert_eq!(counts(&refilling).await, lines(&["ordi 1 0", "pepe 1 0"]));
        })
        .await;
    }
}
