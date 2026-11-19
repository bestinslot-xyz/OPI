//! The history a brc20 database keeps, from `BRC20_HISTORY`: full, or a
//! window of the last [`MAX_ROLLBACK_DEPTH`] blocks that a rollback reads.
//!
//! A window keeps, below that depth, what indexing still reads:
//!
//! - each (pkscript, tick)'s latest `brc20_historic_balances` row, which
//!   balance reads and the rebuild of `brc20_current_balances` after a
//!   rollback take;
//! - the inscribe events of the transfer-style inscriptions (transfer and
//!   the BRC2.0 deploy, call, withdraw and transact) whose transfer event is
//!   absent or inside the window, which transfer validity and the transfer
//!   events read;
//! - the predeploy events, which a self-mint deploy reads by its parent.
//!
//! Every other event, and the `brc20_event_parties` rows of the events it
//! deletes, goes. A spent inscription whose inscribe and transfer events
//! are both gone reads as invalid, which the indexer treats like used.
//! `brc20_used_transfers` keeps the inscription and type of each deleted
//! transfer event, so the API still knows a spent transfer inscription as
//! used (the `cardinal` rule of `/v1/utxos/wallet`).
//!
//! The database records its history in `brc20_history` (no row is full
//! history). A full database switches to a window at start; a windowed one
//! refuses full history, since the deleted history cannot come back.

use std::error::Error;

use opi_core::{History, MAX_ROLLBACK_DEPTH};
use sqlx::{AssertSqlSafe, PgConnection, Pool, Postgres};

/// The event types of the inscriptions a transfer event spends; each one's
/// transfer event has the type one above.
const INSCRIBE_TYPES: &str = "2, 4, 6, 8, 10";
/// The transfer event types, each the type one above its inscribe event's.
const TRANSFER_TYPES: &str = "3, 5, 7, 9, 11";
/// The predeploy inscribe event type.
const PREDEPLOY_TYPE: i32 = 12;

/// Reads the history the database keeps, switches a full database to a
/// window when `history` asks for it and refuses full history on a windowed
/// one.
pub async fn ensure(pool: &Pool<Postgres>, history: History) -> Result<(), Box<dyn Error>> {
    for statement in [
        "CREATE TABLE IF NOT EXISTS brc20_history (history text NOT NULL)",
        "CREATE TABLE IF NOT EXISTS brc20_used_transfers (
             inscription_id text PRIMARY KEY,
             event_type int4 NOT NULL
         )",
    ] {
        sqlx::query(statement).execute(pool).await?;
    }
    let stored: Option<String> = sqlx::query_scalar("SELECT history FROM brc20_history LIMIT 1")
        .fetch_optional(pool)
        .await?;
    match (stored.as_deref(), history) {
        (Some("window"), History::Window) | (None | Some("full"), History::Full) => Ok(()),
        (Some("window"), History::Full) => Err("the brc20 database keeps a history window; \
             BRC20_HISTORY=full needs the database recreated"
            .into()),
        (None | Some("full"), History::Window) => switch_to_window(pool).await,
        (Some(other), _) => Err(format!("brc20_history holds unknown history {other}").into()),
    }
}

/// Deletes, in one transaction, the history a window leaves out at and
/// below `MAX_ROLLBACK_DEPTH` blocks under the cursor. The kept rows of the
/// three history tables move through temporary tables and back after a
/// truncate, which frees the disk at once.
async fn switch_to_window(pool: &Pool<Postgres>) -> Result<(), Box<dyn Error>> {
    tracing::info!(module = "brc20", "switching to a history window");
    let started = std::time::Instant::now();
    let cursor: Option<i32> =
        sqlx::query_scalar("SELECT max(block_height) FROM brc20_block_hashes")
            .fetch_one(pool)
            .await?;
    // the highest height the window leaves out
    let deep = cursor.map_or(-1, |cursor| cursor - MAX_ROLLBACK_DEPTH as i32);
    let mut tx = pool.begin().await?;
    for statement in [
        format!(
            "CREATE TEMPORARY TABLE brc20_events_kept ON COMMIT DROP AS \
             SELECT * FROM brc20_events e \
             WHERE e.block_height > {deep} OR {}",
            kept_below(&deep.to_string())
        ),
        "CREATE TEMPORARY TABLE brc20_event_parties_kept ON COMMIT DROP AS \
         SELECT p.* FROM brc20_event_parties p \
         WHERE p.event_id IN (SELECT id FROM brc20_events_kept)"
            .to_string(),
        format!(
            "CREATE TEMPORARY TABLE brc20_historic_balances_kept ON COMMIT DROP AS \
             SELECT * FROM brc20_historic_balances WHERE block_height > {deep} \
             UNION ALL \
             (SELECT DISTINCT ON (pkscript, tick) * FROM brc20_historic_balances \
              WHERE block_height <= {deep} \
              ORDER BY pkscript, tick, block_height DESC, id DESC)"
        ),
        format!(
            "INSERT INTO brc20_used_transfers (inscription_id, event_type) \
             SELECT inscription_id, event_type FROM brc20_events \
             WHERE block_height <= {deep} AND event_type IN ({TRANSFER_TYPES}) \
             ON CONFLICT DO NOTHING"
        ),
        "TRUNCATE brc20_events, brc20_event_parties, brc20_historic_balances".to_string(),
        "INSERT INTO brc20_events SELECT * FROM brc20_events_kept".to_string(),
        "INSERT INTO brc20_event_parties SELECT * FROM brc20_event_parties_kept".to_string(),
        "INSERT INTO brc20_historic_balances SELECT * FROM brc20_historic_balances_kept"
            .to_string(),
        "DELETE FROM brc20_history".to_string(),
        "INSERT INTO brc20_history (history) VALUES ('window')".to_string(),
    ] {
        sqlx::query(AssertSqlSafe(statement))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::info!(
        module = "brc20",
        deep,
        secs = started.elapsed().as_secs(),
        "switched to a history window"
    );
    Ok(())
}

/// The condition under which an event `e` at or below height `deep` stays:
/// a predeploy, or an inscribe event without its transfer event at or below
/// `deep`.
fn kept_below(deep: &str) -> String {
    format!(
        "e.event_type = {PREDEPLOY_TYPE} \
         OR (e.event_type IN ({INSCRIBE_TYPES}) AND NOT EXISTS ( \
             SELECT 1 FROM brc20_events t \
             WHERE t.inscription_id = e.inscription_id \
               AND t.event_type = e.event_type + 1 \
               AND t.block_height <= {deep}))"
    )
}

/// Deletes the history a window leaves out once block `deep` falls
/// `MAX_ROLLBACK_DEPTH` blocks under the cursor: the events at `deep` the
/// window does not keep, the inscribe events whose transfer event is at
/// `deep`, the party rows of both, and the balance rows at or below `deep`
/// of each (pkscript, tick) changed at `deep` but its latest. The deleted
/// transfer events go to `brc20_used_transfers`.
pub async fn prune(tx: &mut PgConnection, deep: i32) -> Result<(), sqlx::Error> {
    sqlx::query(AssertSqlSafe(format!(
        "WITH doomed AS (
             SELECT e.id, e.event, e.inscription_id, e.event_type FROM brc20_events e
             WHERE e.block_height = $1 AND NOT ({kept})
             UNION ALL
             SELECT i.id, i.event, i.inscription_id, i.event_type FROM brc20_events t
             JOIN brc20_events i
               ON i.inscription_id = t.inscription_id AND i.event_type = t.event_type - 1
             WHERE t.block_height = $1 AND t.event_type IN ({TRANSFER_TYPES})
               AND i.block_height < $1
         ), used AS (
             INSERT INTO brc20_used_transfers (inscription_id, event_type)
             SELECT inscription_id, event_type FROM doomed
             WHERE event_type IN ({TRANSFER_TYPES})
             ON CONFLICT DO NOTHING
         ), parties AS (
             DELETE FROM brc20_event_parties p
             USING (SELECT d.id, k FROM doomed d
                    CROSS JOIN LATERAL unnest(brc20_event_party_pkscripts(d.event)) AS k) x
             WHERE p.pkscript = x.k AND p.event_id = x.id
         )
         DELETE FROM brc20_events e USING doomed d WHERE e.id = d.id",
        kept = kept_below("$1")
    )))
    .bind(deep)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM brc20_historic_balances o
         USING (SELECT pkscript, tick, max(id) AS keep FROM brc20_historic_balances
                WHERE block_height = $1 GROUP BY pkscript, tick) k
         WHERE o.pkscript = k.pkscript AND o.tick = k.tick
           AND o.block_height <= $1 AND o.id <> k.keep",
    )
    .bind(deep)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::{
        Brc20ProgCallInscribeEvent, Brc20ProgCallTransferEvent, Brc20ProgDeployInscribeEvent,
        Brc20ProgDeployTransferEvent, Brc20ProgTransactInscribeEvent,
        Brc20ProgTransactTransferEvent, Brc20ProgWithdrawInscribeEvent,
        Brc20ProgWithdrawTransferEvent, Event, PreDeployInscribeEvent, TransferInscribeEvent,
        TransferTransferEvent,
    };

    #[test]
    fn the_type_lists_match_the_events() {
        let pairs = [
            (
                TransferInscribeEvent::event_id(),
                TransferTransferEvent::event_id(),
            ),
            (
                Brc20ProgDeployInscribeEvent::event_id(),
                Brc20ProgDeployTransferEvent::event_id(),
            ),
            (
                Brc20ProgCallInscribeEvent::event_id(),
                Brc20ProgCallTransferEvent::event_id(),
            ),
            (
                Brc20ProgWithdrawInscribeEvent::event_id(),
                Brc20ProgWithdrawTransferEvent::event_id(),
            ),
            (
                Brc20ProgTransactInscribeEvent::event_id(),
                Brc20ProgTransactTransferEvent::event_id(),
            ),
        ];
        let join = |ids: Vec<i32>| {
            ids.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert_eq!(
            join(pairs.iter().map(|(inscribe, _)| *inscribe).collect()),
            INSCRIBE_TYPES
        );
        assert_eq!(
            join(pairs.iter().map(|(_, transfer)| *transfer).collect()),
            TRANSFER_TYPES
        );
        for (inscribe, transfer) in pairs {
            assert_eq!(transfer, inscribe + 1);
        }
        assert_eq!(PreDeployInscribeEvent::event_id(), PREDEPLOY_TYPE);
    }
}

#[cfg(test)]
mod db_tests {
    use bitcoin::Network;
    use sqlx::{Connection, postgres::PgConnectOptions};

    use super::*;
    use crate::{config::Brc20IndexerConfig, database::Brc20Database};

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
        let name = format!("opi_brc20_window_{tag}_{}_{nanos}", std::process::id());
        let admin = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("alper")
            .database("postgres");
        let mut root = sqlx::PgConnection::connect_with(&admin).await.unwrap();
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut root)
            .await
            .unwrap();
        let result = tokio::spawn(body(name.clone())).await;
        sqlx::query(AssertSqlSafe(format!("DROP DATABASE {name} WITH (FORCE)")))
            .execute(&mut root)
            .await
            .unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error.into_panic());
        }
    }

    fn config(name: &str, network: Network, history: History) -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: network,
            history,
            ..Default::default()
        }
    }

    async fn database(name: &str, history: History) -> Brc20Database {
        let mut database = Brc20Database::new(&config(name, Network::Signet, history));
        database.init().await.unwrap();
        database
    }

    /// Writes the events, balance rows and party rows the tests prune, and
    /// block hashes up to `cursor`:
    ///
    /// | id | type | height | inscription |
    /// |----|------|--------|-------------|
    /// | 1  | deploy   | 10 | a |
    /// | 2  | mint     | 10 | b |
    /// | 3  | transfer inscribe, never spent | 10 | c |
    /// | 4  | transfer inscribe | 10 | d |
    /// | 5  | transfer, same block | 10 | d |
    /// | 6  | BRC2.0 call inscribe | 10 | e |
    /// | 7  | BRC2.0 call transfer | 20 | e |
    /// | 8  | predeploy | 10 | f |
    /// | 9  | mint     | 20 | g |
    ///
    /// Balance rows `id pkscript tick height`: `1 p1 ordi 5`, `2 p1 ordi 10`,
    /// `3 p1 ordi 10`, `4 p2 ordi 5`, `5 p1 pepe 10`, `6 p1 pepe 15`.
    async fn seed(pool: &Pool<Postgres>, cursor: i32) {
        for (id, event_type, height, inscription) in [
            (1, 0, 10, "a"),
            (2, 1, 10, "b"),
            (3, 2, 10, "c"),
            (4, 2, 10, "d"),
            (5, 3, 10, "d"),
            (6, 6, 10, "e"),
            (7, 7, 20, "e"),
            (8, 12, 10, "f"),
            (9, 1, 20, "g"),
        ] {
            sqlx::query(
                "INSERT INTO brc20_events (id, event_type, block_height, inscription_id, event)
                 VALUES ($1, $2, $3, $4, jsonb_build_object('source_pkScript', $5::text))",
            )
            .bind(id as i64)
            .bind(event_type)
            .bind(height)
            .bind(inscription)
            .bind(format!("00{id:02x}"))
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO brc20_event_parties (pkscript, event_id)
             SELECT k, e.id FROM brc20_events e
             CROSS JOIN LATERAL unnest(brc20_event_party_pkscripts(e.event)) AS k",
        )
        .execute(pool)
        .await
        .unwrap();
        for (id, pkscript, tick, height) in [
            (1, "p1", "ordi", 5),
            (2, "p1", "ordi", 10),
            (3, "p1", "ordi", 10),
            (4, "p2", "ordi", 5),
            (5, "p1", "pepe", 10),
            (6, "p1", "pepe", 15),
        ] {
            sqlx::query(
                "INSERT INTO brc20_historic_balances
                     (id, pkscript, wallet, tick, overall_balance, available_balance, block_height)
                 VALUES ($1, $2, '', $3, $1, $1, $4)",
            )
            .bind(id as i64)
            .bind(pkscript)
            .bind(tick)
            .bind(height)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO brc20_block_hashes (block_height, block_hash)
             SELECT h, 'hash' || h FROM generate_series(0, $1) AS h",
        )
        .bind(cursor)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn ids(pool: &Pool<Postgres>, sql: &str) -> Vec<i64> {
        sqlx::query_scalar(AssertSqlSafe(sql.to_string()))
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn event_ids(pool: &Pool<Postgres>) -> Vec<i64> {
        ids(pool, "SELECT id FROM brc20_events ORDER BY id").await
    }

    async fn party_ids(pool: &Pool<Postgres>) -> Vec<i64> {
        ids(
            pool,
            "SELECT event_id FROM brc20_event_parties ORDER BY event_id",
        )
        .await
    }

    async fn balance_ids(pool: &Pool<Postgres>) -> Vec<i64> {
        ids(pool, "SELECT id FROM brc20_historic_balances ORDER BY id").await
    }

    /// The rows of `brc20_used_transfers` as `inscription event_type`.
    async fn used(pool: &Pool<Postgres>) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT inscription_id || ' ' || event_type FROM brc20_used_transfers
             ORDER BY inscription_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn prune_at(pool: &Pool<Postgres>, deep: i32) {
        let mut tx = pool.begin().await.unwrap();
        prune(&mut tx, deep).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn a_window_keeps_unspent_inscribes_predeploys_and_latest_balances() {
        with_database("prune", |name| async move {
            let database = database(&name, History::Full).await;
            let pool = &database.client;
            seed(pool, 20).await;

            prune_at(pool, 10).await;
            // the deploy, the mint and the spent pair at 10 go; the unspent
            // inscribe, the inscribe spent at 20 and the predeploy stay
            assert_eq!(event_ids(pool).await, [3, 6, 7, 8, 9]);
            assert_eq!(party_ids(pool).await, [3, 6, 7, 8, 9]);
            // p1 ordi keeps its latest row at 10, p2 ordi is untouched, and
            // p1 pepe keeps its row at 10 for a rollback below 15
            assert_eq!(balance_ids(pool).await, [3, 4, 5, 6]);
            // the deleted transfer stays known as used
            assert_eq!(used(pool).await, ["d 3"]);

            prune_at(pool, 15).await;
            assert_eq!(balance_ids(pool).await, [3, 4, 6]);

            prune_at(pool, 20).await;
            // the pair spent at 20 and the mint at 20 go
            assert_eq!(event_ids(pool).await, [3, 8]);
            assert_eq!(party_ids(pool).await, [3, 8]);
            assert_eq!(used(pool).await, ["d 3", "e 7"]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_full_database_switches_to_a_window_and_not_back() {
        with_database("switch", |name| async move {
            let pool = database(&name, History::Full).await.client;
            // the cursor 1010 leaves heights up to 10 out of the window
            seed(&pool, 10 + MAX_ROLLBACK_DEPTH as i32).await;

            ensure(&pool, History::Window).await.unwrap();
            assert_eq!(event_ids(&pool).await, [3, 6, 7, 8, 9]);
            assert_eq!(party_ids(&pool).await, [3, 6, 7, 8, 9]);
            assert_eq!(balance_ids(&pool).await, [3, 4, 5, 6]);
            assert_eq!(used(&pool).await, ["d 3"]);

            ensure(&pool, History::Window).await.unwrap();
            let error = ensure(&pool, History::Full).await.unwrap_err();
            assert!(error.to_string().contains("window"), "{error}");
        })
        .await;
    }

    #[tokio::test]
    async fn a_windowed_database_prunes_as_it_writes_each_block() {
        with_database("block", |name| async move {
            let mut database = database(&name, History::Window).await;
            seed(&database.client, 10 + MAX_ROLLBACK_DEPTH as i32 - 1).await;

            database
                .set_block_hashes(10 + MAX_ROLLBACK_DEPTH as i32, "hash", "", None, &[])
                .await
                .unwrap();
            assert_eq!(event_ids(&database.client).await, [3, 6, 7, 8, 9]);
            assert_eq!(balance_ids(&database.client).await, [3, 4, 5, 6]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_window_skips_the_refund_order_fix_of_pruned_events() {
        with_database("refund", |name| async move {
            let refund_height = 932_888;
            for history in [History::Full, History::Window] {
                let mut database = Brc20Database::new(&config(&name, Network::Bitcoin, history));
                database.init().await.unwrap();
                sqlx::query("DELETE FROM brc20_block_hashes")
                    .execute(&database.client)
                    .await
                    .unwrap();
                sqlx::query(
                    "INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES ($1, 'h')",
                )
                .bind(refund_height + MAX_ROLLBACK_DEPTH as i32)
                .execute(&database.client)
                .await
                .unwrap();
                let fixed = database.maybe_fix_refund_order(refund_height).await;
                match history {
                    History::Full => assert!(fixed.is_err()),
                    History::Window => assert!(!fixed.unwrap()),
                }
            }
        })
        .await;
    }
}
