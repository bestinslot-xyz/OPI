//! The indexes behind the API's event feeds, `/v1/brc20/ticker/events` and
//! `/v1/brc20/wallet/events`, on `brc20_events` and `brc20_light_events`
//! (only one of the two is written, by the indexer's mode).
//!
//! - `*_ticker_id_idx` on `(event ->> 'tick', id)` serves a ticker's events
//!   newest first: an index range scan walks the ticker's ids backwards and
//!   stops after the page.
//! - `*_parties_idx` is a GIN index on the array of every party of an event:
//!   its pkScripts (`deployer_pkScript`, `minted_pkScript`,
//!   `source_pkScript`, `spent_pkScript`, `predeployer_pkScript`) and its
//!   wallets (`*_wallet`), without nulls and empty strings. A wallet's events
//!   are the rows whose array overlaps `ARRAY[wallet]`. One index covers
//!   every event type and both kinds of party; the array holds one to four
//!   entries per event.
//!
//! Each index is created at start when missing, and the same statements are
//! in db_init.sql. Nothing here feeds an event, hash or report.

use sqlx::{Pool, Postgres};

/// The parties of an event, as the indexes and the API's queries spell it.
macro_rules! parties {
    () => {
        "array_remove(ARRAY[nullif((\"event\" ->> 'deployer_pkScript'), ''), \
         nullif((\"event\" ->> 'minted_pkScript'), ''), \
         nullif((\"event\" ->> 'source_pkScript'), ''), \
         nullif((\"event\" ->> 'spent_pkScript'), ''), \
         nullif((\"event\" ->> 'predeployer_pkScript'), ''), \
         nullif((\"event\" ->> 'deployer_wallet'), ''), \
         nullif((\"event\" ->> 'minted_wallet'), ''), \
         nullif((\"event\" ->> 'source_wallet'), ''), \
         nullif((\"event\" ->> 'spent_wallet'), ''), \
         nullif((\"event\" ->> 'predeployer_wallet'), '')], NULL)"
    };
}

/// The statements of db_init.sql.
pub const SCHEMA_SQL: &str = concat!(
    "CREATE INDEX IF NOT EXISTS brc20_events_ticker_id_idx ON public.brc20_events USING btree (((\"event\" ->> 'tick'::text)), id);\n",
    "CREATE INDEX IF NOT EXISTS brc20_events_parties_idx ON public.brc20_events USING gin ((",
    parties!(),
    "));\n",
    "CREATE INDEX IF NOT EXISTS brc20_light_events_ticker_id_idx ON public.brc20_light_events USING btree (((\"event\" ->> 'tick'::text)), id);\n",
    "CREATE INDEX IF NOT EXISTS brc20_light_events_parties_idx ON public.brc20_light_events USING gin ((",
    parties!(),
    "));\n",
);

/// The indexes by name with their statements, each created at start when
/// missing.
pub const INDEXES: [(&str, &str); 4] = [
    (
        "brc20_events_ticker_id_idx",
        "CREATE INDEX IF NOT EXISTS brc20_events_ticker_id_idx ON public.brc20_events USING btree (((\"event\" ->> 'tick'::text)), id)",
    ),
    (
        "brc20_events_parties_idx",
        concat!(
            "CREATE INDEX IF NOT EXISTS brc20_events_parties_idx ON public.brc20_events USING gin ((",
            parties!(),
            "))"
        ),
    ),
    (
        "brc20_light_events_ticker_id_idx",
        "CREATE INDEX IF NOT EXISTS brc20_light_events_ticker_id_idx ON public.brc20_light_events USING btree (((\"event\" ->> 'tick'::text)), id)",
    ),
    (
        "brc20_light_events_parties_idx",
        concat!(
            "CREATE INDEX IF NOT EXISTS brc20_light_events_parties_idx ON public.brc20_light_events USING gin ((",
            parties!(),
            "))"
        ),
    ),
];

/// The indexes on `(event ->> 'tick')` alone, dropped at start once the
/// `(tick, id)` indexes exist.
pub const SUPERSEDED: [&str; 2] = ["brc20_events_ticker_idx", "brc20_light_events_ticker_idx"];

/// Creates each missing index. The pg_indexes lookup comes first, so a start
/// that finds the index never waits on the table lock.
pub async fn create_if_missing(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    for (name, sql) in INDEXES {
        let exists =
            sqlx::query("SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1")
                .bind(name)
                .fetch_optional(pool)
                .await?
                .is_some();
        if !exists {
            tracing::info!("Creating index {name}");
            sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
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
    /// keys of its party fields.
    fn party_keys<T: serde::Serialize + serde::de::DeserializeOwned>(
        superset: &serde_json::Value,
    ) -> Vec<String> {
        let event: T = serde_json::from_value(superset.clone()).unwrap();
        serde_json::to_value(&event)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.ends_with("_pkScript") || key.ends_with("_wallet"))
            .cloned()
            .collect()
    }

    #[test]
    fn every_party_field_of_every_event_type_is_indexed() {
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
        keys.extend(party_keys::<DeployInscribeEvent>(&superset));
        keys.extend(party_keys::<PreDeployInscribeEvent>(&superset));
        keys.extend(party_keys::<MintInscribeEvent>(&superset));
        keys.extend(party_keys::<TransferInscribeEvent>(&superset));
        keys.extend(party_keys::<TransferTransferEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgDeployInscribeEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgDeployTransferEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgCallInscribeEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgCallTransferEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgWithdrawInscribeEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgWithdrawTransferEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgTransactInscribeEvent>(&superset));
        keys.extend(party_keys::<Brc20ProgTransactTransferEvent>(&superset));
        assert!(keys.len() >= 20, "{keys:?}");
        let indexed = parties!();
        for key in keys {
            assert!(
                indexed.contains(&format!("->> '{key}'")),
                "{key} is not an indexed party"
            );
        }
    }

    #[test]
    fn db_init_creates_the_indexes() {
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        assert!(init.contains(SCHEMA_SQL));
        for (name, sql) in INDEXES {
            assert!(SCHEMA_SQL.contains(&format!("{sql};")), "{name}");
            assert!(sql.contains(name), "{name}");
        }
    }
}

/// Postgres-backed test on a throwaway database. It skips unless
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
               AND (indexname LIKE '%ticker_id_idx' OR indexname LIKE '%parties_idx')
             ORDER BY indexname",
        )
        .fetch_all(&database.client)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_start_creates_the_missing_indexes() {
        if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
            eprintln!("OPI_TEST_PG is not 1; skipping");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("opi_brc20_feed_idx_{}_{nanos}", std::process::id());
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
        let db_name = name.clone();
        let result = tokio::spawn(async move {
            let expected: Vec<String> = [
                "brc20_events_parties_idx",
                "brc20_events_ticker_id_idx",
                "brc20_light_events_parties_idx",
                "brc20_light_events_ticker_id_idx",
            ]
            .map(String::from)
            .into();
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
            sqlx::raw_sql(
                "DROP INDEX brc20_events_parties_idx;
                 DROP INDEX brc20_events_ticker_id_idx;
                 DROP INDEX brc20_light_events_parties_idx;
                 DROP INDEX brc20_light_events_ticker_id_idx;
                 CREATE INDEX brc20_events_ticker_idx ON public.brc20_events USING btree (((\"event\" ->> 'tick'::text)));
                 CREATE INDEX brc20_light_events_ticker_idx ON public.brc20_light_events USING btree (((\"event\" ->> 'tick'::text)));",
            )
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
