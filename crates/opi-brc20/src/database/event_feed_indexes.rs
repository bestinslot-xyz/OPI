//! The indexes behind the API's event feeds, `/v1/brc20/ticker/events`,
//! `/v1/brc20/wallet/events` and `/v1/brc20/deploys`, on `brc20_events` and
//! `brc20_light_events` (only one of the two is written, by the indexer's
//! mode).
//!
//! - `*_ticker_id_idx` on `(event ->> 'tick', id)` serves a ticker's events
//!   newest first: an index range scan walks the ticker's ids backwards and
//!   stops after the page.
//! - `*_<category>_ticker_id_idx` is the same index over one rare category
//!   (`deploy`, `wrap`, `unwrap`), so a ticker's page of that category reads
//!   only its rows. BRC2.0 deploys, calls and transactions carry no ticker.
//! - `*_<category>_id_idx` on `(id)` over one rare category (`deploy`,
//!   `wrap`, `unwrap`, `brc20prog`) serves the network-wide list of that
//!   category newest first.
//!
//! A category is `brc20_event_category(event_type, event)` (db_init.sql),
//! and the feed queries spell their filter as
//! `brc20_event_category(event_type, event) = ANY($n)`, which a single
//! category turns into the partial indexes' predicate. A wallet's events
//! come from `brc20_event_parties` instead (see [`brc20_database`]).
//!
//! Each index is created at start when missing, and the same statements are
//! in db_init.sql. Nothing here feeds an event, hash or report.
//!
//! [`brc20_database`]: crate::database::brc20_database

use sqlx::{Pool, Postgres};

/// The `CREATE INDEX` statement of index `$name` on `$table`.
macro_rules! index {
    ($table:literal, $name:literal, $columns:literal) => {
        concat!(
            "CREATE INDEX IF NOT EXISTS ",
            $table,
            "_",
            $name,
            " ON public.",
            $table,
            " USING btree ",
            $columns
        )
    };
    ($table:literal, $name:literal, $columns:literal, $category:literal) => {
        concat!(
            index!($table, $name, $columns),
            " WHERE (public.brc20_event_category(event_type, \"event\") = '",
            $category,
            "'::text)"
        )
    };
}

/// The feed indexes of one event table, by name with their statements.
macro_rules! table_indexes {
    ($table:literal) => {
        [
            (
                concat!($table, "_ticker_id_idx"),
                index!(
                    $table,
                    "ticker_id_idx",
                    "(((\"event\" ->> 'tick'::text)), id)"
                ),
            ),
            (
                concat!($table, "_deploy_ticker_id_idx"),
                index!(
                    $table,
                    "deploy_ticker_id_idx",
                    "(((\"event\" ->> 'tick'::text)), id)",
                    "deploy"
                ),
            ),
            (
                concat!($table, "_wrap_ticker_id_idx"),
                index!(
                    $table,
                    "wrap_ticker_id_idx",
                    "(((\"event\" ->> 'tick'::text)), id)",
                    "wrap"
                ),
            ),
            (
                concat!($table, "_unwrap_ticker_id_idx"),
                index!(
                    $table,
                    "unwrap_ticker_id_idx",
                    "(((\"event\" ->> 'tick'::text)), id)",
                    "unwrap"
                ),
            ),
            (
                concat!($table, "_deploy_id_idx"),
                index!($table, "deploy_id_idx", "(id)", "deploy"),
            ),
            (
                concat!($table, "_wrap_id_idx"),
                index!($table, "wrap_id_idx", "(id)", "wrap"),
            ),
            (
                concat!($table, "_unwrap_id_idx"),
                index!($table, "unwrap_id_idx", "(id)", "unwrap"),
            ),
            (
                concat!($table, "_brc20prog_id_idx"),
                index!($table, "brc20prog_id_idx", "(id)", "brc20prog"),
            ),
        ]
    };
}

/// The indexes by name with their statements, each created at start when
/// missing.
pub const INDEXES: [[(&str, &str); 8]; 2] = [
    table_indexes!("brc20_events"),
    table_indexes!("brc20_light_events"),
];

/// The indexes on `(event ->> 'tick')` alone, dropped at start once the
/// `(tick, id)` indexes exist.
pub const SUPERSEDED: [&str; 2] = ["brc20_events_ticker_idx", "brc20_light_events_ticker_idx"];

/// Creates each missing index. The pg_indexes lookup comes first, so a start
/// that finds the index never waits on the table lock.
pub async fn create_if_missing(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    for (name, sql) in INDEXES.iter().flatten() {
        let exists =
            sqlx::query("SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1")
                .bind(name)
                .fetch_optional(pool)
                .await?
                .is_some();
        if !exists {
            tracing::info!("Creating index {name}");
            sqlx::query(sqlx::AssertSqlSafe(*sql)).execute(pool).await?;
        }
    }
    // The `(tick)` indexes are prefixes of the `(tick, id)` ones.
    for name in SUPERSEDED {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP INDEX IF EXISTS public.{name}"
        )))
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes an event of type `T` built from `superset` and returns the
    /// keys of its pkScript fields.
    fn pkscript_keys<T: serde::Serialize + serde::de::DeserializeOwned>(
        superset: &serde_json::Value,
    ) -> Vec<String> {
        let event: T = serde_json::from_value(superset.clone()).unwrap();
        serde_json::to_value(&event)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.ends_with("_pkScript"))
            .cloned()
            .collect()
    }

    #[test]
    fn every_pkscript_field_of_every_event_type_is_a_party() {
        use crate::types::events::*;
        let mut superset = serde_json::json!({
            "tick": "t", "original_tick": "T", "amount": "1", "decimals": "18",
            "max_supply": "1", "limit_per_mint": "1", "is_self_mint": "false",
            "hash": "h", "block_height": "1", "parent_id": "p", "using_tx_id": "u",
            "contract_address": "c", "contract_inscription_id": "c",
            "data": "d", "base64_data": "d", "byte_len": "0", "btc_txId": "b",
        });
        for prefix in ["deployer", "minted", "source", "spent", "predeployer"] {
            superset[format!("{prefix}_pkScript")] = "00".into();
            superset[format!("{prefix}_wallet")] = "w".into();
        }
        let mut keys = Vec::new();
        keys.extend(pkscript_keys::<DeployInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<PreDeployInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<MintInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<TransferInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<TransferTransferEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgDeployInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgDeployTransferEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgCallInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgCallTransferEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgWithdrawInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgWithdrawTransferEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgTransactInscribeEvent>(&superset));
        keys.extend(pkscript_keys::<Brc20ProgTransactTransferEvent>(&superset));
        assert!(keys.len() >= 15, "{keys:?}");
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        let function = &init[init
            .find("FUNCTION public.brc20_event_party_pkscripts")
            .unwrap()..];
        let function = &function[..function.find("$$;").unwrap()];
        for key in keys {
            assert!(
                function.contains(&format!("e ->> '{key}'")),
                "{key} is not a party"
            );
        }
    }

    #[test]
    fn db_init_creates_the_indexes() {
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        for (name, sql) in INDEXES.iter().flatten() {
            assert!(init.contains(&format!("{sql};\n")), "{name}");
            assert!(
                sql.contains(&format!("INDEX IF NOT EXISTS {name} ON")),
                "{name}"
            );
        }
    }
}

/// Postgres-backed tests on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use bitcoin::Network;
    use sqlx::{Connection, postgres::PgConnectOptions};

    use crate::{config::Brc20IndexerConfig, database::Brc20Database};

    async fn start(name: &str) -> Brc20Database {
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

    async fn present(database: &Brc20Database) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT indexname FROM pg_indexes
             WHERE schemaname = 'public' AND indexname LIKE ANY (ARRAY['brc20_events_%_idx', 'brc20_light_events_%_idx'])
               AND indexname LIKE '%id_idx' AND indexname NOT LIKE '%inscription_id%'
             ORDER BY indexname",
        )
        .fetch_all(&database.client)
        .await
        .unwrap()
    }

    /// Runs `body` on a throwaway database, dropped even when `body` panics.
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
        let name = format!("opi_brc20_feed_{tag}_{}_{nanos}", std::process::id());
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

    #[tokio::test]
    async fn a_start_creates_the_missing_indexes() {
        with_database("idx", |db_name| async move {
            let mut expected: Vec<String> = super::INDEXES
                .iter()
                .flatten()
                .map(|(name, _)| name.to_string())
                .collect();
            expected.sort();
            let old = |database: &Brc20Database| {
                let client = database.client.clone();
                async move {
                    sqlx::query_scalar::<_, String>(
                        "SELECT indexname FROM pg_indexes
                         WHERE indexname IN ('brc20_events_ticker_idx', 'brc20_light_events_ticker_idx')",
                    )
                    .fetch_all(&client)
                    .await
                    .unwrap()
                }
            };
            let first = start(&db_name).await;
            assert_eq!(present(&first).await, expected);
            assert!(old(&first).await.is_empty());

            // A database with the (tick) indexes and without the feed
            // indexes has the feed indexes and not the (tick) ones after a
            // start.
            let mut drop = String::new();
            for name in &expected {
                drop.push_str(&format!("DROP INDEX {name};\n"));
            }
            drop.push_str(
                "CREATE INDEX brc20_events_ticker_idx ON public.brc20_events USING btree (((\"event\" ->> 'tick'::text)));
                 CREATE INDEX brc20_light_events_ticker_idx ON public.brc20_light_events USING btree (((\"event\" ->> 'tick'::text)));",
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(drop))
                .execute(&first.client)
                .await
                .unwrap();
            assert!(present(&first).await.is_empty());
            assert_eq!(old(&first).await.len(), 2);
            let second = start(&db_name).await;
            assert_eq!(present(&second).await, expected);
            assert!(old(&second).await.is_empty());
            // and a start that finds them changes nothing
            let third = start(&db_name).await;
            assert_eq!(present(&third).await, expected);
            assert!(old(&third).await.is_empty());
        })
        .await;
    }

    /// The party rows of both event tables: written with the events, one per
    /// party pkScript, and deleted with them on a reorg.
    #[tokio::test]
    async fn the_parties_follow_the_events_through_a_reorg() {
        use crate::database::brc20_database::{EventRecord, LightEventRecord};
        with_database("parties", |db_name| async move {
            let mut database = start(&db_name).await;
            // ids from 1, as the sequences give them
            database.current_event_id = 1;
            let parties = |database: &Brc20Database| {
                let client = database.client.clone();
                async move {
                    sqlx::query_as::<_, (String, i64)>(
                        "SELECT encode(pkscript, 'hex'), event_id FROM brc20_event_parties
                         ORDER BY event_id, pkscript",
                    )
                    .fetch_all(&client)
                    .await
                    .unwrap()
                }
            };
            let mut push = |height: i32, event: serde_json::Value, light: bool| {
                let event_id = database.current_event_id;
                if light {
                    database.light_event_inserts.push(LightEventRecord {
                        event_id,
                        event_type_id: 3,
                        block_height: height,
                        inscription_id: format!("{event_id}i0"),
                        event,
                    });
                } else {
                    database.event_inserts.push(EventRecord {
                        event_id,
                        event_type_id: 3,
                        block_height: height,
                        inscription_id: format!("{event_id}i0"),
                        event,
                    });
                }
                database.current_event_id += 1;
            };
            // a transfer between two pkScripts, one to itself, and a light
            // transfer spent as a fee
            push(
                10,
                serde_json::json!({"source_pkScript": "0014a1", "spent_pkScript": "0014b2"}),
                false,
            );
            push(
                11,
                serde_json::json!({"source_pkScript": "0014a1", "spent_pkScript": "0014a1"}),
                false,
            );
            push(
                11,
                serde_json::json!({"source_pkScript": "0014c3", "spent_pkScript": null}),
                true,
            );
            database.flush_queries_to_db().await.unwrap();
            let all = vec![
                ("0014a1".to_string(), 1),
                ("0014b2".to_string(), 1),
                ("0014a1".to_string(), 2),
                ("0014c3".to_string(), 3),
            ];
            assert_eq!(parties(&database).await, all);
            database.reorg(10).await.unwrap();
            assert_eq!(parties(&database).await, all[..2]);
            database.reorg(9).await.unwrap();
            assert!(parties(&database).await.is_empty());
        })
        .await;
    }

    /// The category of each event type, and the transfer-transfer split by
    /// its spent pkScript.
    #[tokio::test]
    async fn every_event_type_has_one_category() {
        with_database("cat", |db_name| async move {
            let database = start(&db_name).await;
            let rows: Vec<(i32, Option<String>)> = sqlx::query_as(
                "SELECT t.event_type_id, brc20_event_category(t.event_type_id, '{}'::jsonb)
                 FROM brc20_event_types t ORDER BY t.event_type_id",
            )
            .fetch_all(&database.client)
            .await
            .unwrap();
            let categories: Vec<(i32, &str)> = rows
                .iter()
                .map(|(id, category)| (*id, category.as_deref().unwrap()))
                .collect();
            assert_eq!(
                categories,
                [
                    (0, "deploy"),
                    (1, "mint"),
                    (2, "transfer"),
                    (3, "transfer"),
                    (4, "brc20prog"),
                    (5, "brc20prog"),
                    (6, "brc20prog"),
                    (7, "brc20prog"),
                    (8, "unwrap"),
                    (9, "unwrap"),
                    (10, "brc20prog"),
                    (11, "brc20prog"),
                    (12, "deploy"),
                ]
            );
            let wrap: String = sqlx::query_scalar(
                "SELECT brc20_event_category(3, jsonb_build_object('spent_pkScript', $1::text))",
            )
            .bind(crate::config::BRC20_PROG_OP_RETURN_PKSCRIPT)
            .fetch_one(&database.client)
            .await
            .unwrap();
            assert_eq!(wrap, "wrap");
            let parties: Vec<Vec<u8>> = sqlx::query_scalar(
                "SELECT unnest(brc20_event_party_pkscripts(
                     '{\"source_pkScript\":\"0014a1\",\"spent_pkScript\":\"0014a1\",
                       \"minted_pkScript\":\"\",\"deployer_pkScript\":\"0014AB\",
                       \"predeployer_pkScript\":\"6a\"}'::jsonb)) ORDER BY 1",
            )
            .fetch_all(&database.client)
            .await
            .unwrap();
            // one row per pkScript; empty and uppercase values are no party
            assert_eq!(parties, [vec![0x00, 0x14, 0xa1], vec![0x6a]]);
        })
        .await;
    }
}
