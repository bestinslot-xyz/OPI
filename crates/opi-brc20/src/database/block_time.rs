//! The `block_time` column of `brc20_block_hashes`: each block's time in
//! seconds since the epoch.
//!
//! Every block write sets it. A database that predates the column gets it at
//! start, and a full-mode indexer fills the rows without a time from the
//! journal's block headers before it indexes again. The fill selects the rows
//! whose `block_time` is NULL, so it commits batch by batch and an interrupted
//! fill resumes where it stopped.

use std::error::Error;

use opi_core::BlockSource;
use sqlx::{Pool, Postgres};

use super::Brc20Database;

/// Rows per fill transaction.
pub const FILL_BATCH: usize = 10_000;

/// Adds the `block_time` column when the table lacks it. The check comes first
/// so a process that finds the column never waits on the table lock.
pub async fn add_column_if_missing(client: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    let present = sqlx::query(
        "SELECT 1 FROM information_schema.columns
         WHERE table_name = 'brc20_block_hashes' AND column_name = 'block_time'
           AND table_schema = current_schema()",
    )
    .fetch_optional(client)
    .await?
    .is_some();
    if !present {
        sqlx::query("ALTER TABLE brc20_block_hashes ADD COLUMN IF NOT EXISTS block_time int8")
            .execute(client)
            .await?;
    }
    Ok(())
}

impl Brc20Database {
    /// Sets `block_time` of every stored block that has none and that the
    /// journal knows, `batch` rows per transaction, and returns how many rows
    /// it set. Heights the journal does not hold keep a NULL time.
    pub async fn fill_block_times(
        &self,
        journal: &dyn BlockSource,
        batch: usize,
    ) -> Result<u64, Box<dyn Error>> {
        let mut filled = 0u64;
        let mut after = i32::MIN;
        loop {
            let heights: Vec<i32> = sqlx::query_scalar(
                "SELECT block_height FROM brc20_block_hashes
                 WHERE block_time IS NULL AND block_height > $1
                 ORDER BY block_height LIMIT $2",
            )
            .bind(after)
            .bind(batch as i64)
            .fetch_all(&self.client)
            .await?;
            let Some(&last) = heights.last() else {
                return Ok(filled);
            };
            after = last;

            let mut known = Vec::with_capacity(heights.len());
            let mut times = Vec::with_capacity(heights.len());
            for height in heights {
                if let Some(info) = journal.block_info(height as u32)? {
                    known.push(height);
                    times.push(i64::from(info.time));
                }
            }
            if known.is_empty() {
                continue;
            }
            filled += sqlx::query(
                "UPDATE brc20_block_hashes AS b SET block_time = v.block_time
                 FROM unnest($1::int4[], $2::int8[]) AS v(block_height, block_time)
                 WHERE b.block_height = v.block_height AND b.block_time IS NULL",
            )
            .bind(&known)
            .bind(&times)
            .execute(&self.client)
            .await?
            .rows_affected();
        }
    }
}

/// Postgres-backed tests, each on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod tests {
    use bitcoin::Network;
    use opi_core::{BlockInfo, BlockView, InscriptionInfo, InscriptionRecord};
    use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};

    use super::*;
    use crate::config::Brc20IndexerConfig;

    fn admin() -> PgConnectOptions {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("alper")
            .database("postgres")
    }

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
        let name = format!("opi_brc20_bt_{tag}_{}_{nanos}", std::process::id());
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

    fn config(name: &str, light_client_mode: bool) -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: Network::Signet,
            light_client_mode,
            ..Default::default()
        }
    }

    async fn database(name: &str, light_client_mode: bool) -> Brc20Database {
        let mut database = Brc20Database::new(&config(name, light_client_mode));
        database.init().await.unwrap();
        database
    }

    async fn times(database: &Brc20Database) -> Vec<(i32, Option<i64>)> {
        sqlx::query_as(
            "SELECT block_height, block_time FROM brc20_block_hashes ORDER BY block_height",
        )
        .fetch_all(&database.client)
        .await
        .unwrap()
    }

    async fn insert_block(database: &Brc20Database, height: i32, time: Option<i64>) {
        sqlx::query("INSERT INTO brc20_block_hashes (block_height, block_hash, block_time) VALUES ($1, $2, $3)")
            .bind(height)
            .bind(format!("hash{height}"))
            .bind(time)
            .execute(&database.client)
            .await
            .unwrap();
    }

    /// A journal whose block at `height` has time `1000 + height`, up to
    /// `tip`.
    struct Journal {
        tip: u32,
    }

    impl BlockSource for Journal {
        fn tip(&self) -> anyhow::Result<Option<u32>> {
            Ok(Some(self.tip))
        }

        fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>> {
            Ok((height <= self.tip).then(|| BlockInfo {
                height,
                hash: format!("hash{height}"),
                time: 1000 + height,
            }))
        }

        fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
            Ok(Vec::new())
        }

        fn inscription_info(&self, _: &str) -> anyhow::Result<Option<InscriptionInfo>> {
            Ok(None)
        }

        fn block_view(&self, _: u32) -> anyhow::Result<Option<BlockView>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn a_new_database_has_the_column() {
        with_database("new", |name| async move {
            let database = database(&name, false).await;
            let nullable: String = sqlx::query_scalar(
                "SELECT is_nullable FROM information_schema.columns
                 WHERE table_name = 'brc20_block_hashes' AND column_name = 'block_time'",
            )
            .fetch_one(&database.client)
            .await
            .unwrap();
            assert_eq!(nullable, "YES");
        })
        .await;
    }

    #[tokio::test]
    async fn start_adds_the_column_to_an_existing_database() {
        with_database("add", |name| async move {
            let database = database(&name, false).await;
            insert_block(&database, 5, Some(77)).await;
            sqlx::query("ALTER TABLE brc20_block_hashes DROP COLUMN block_time")
                .execute(&database.client)
                .await
                .unwrap();

            let mut again = Brc20Database::new(&config(&name, false));
            again.init().await.unwrap();
            // A second start finds the column and changes nothing.
            again.init().await.unwrap();
            assert_eq!(times(&again).await, vec![(5, None)]);
        })
        .await;
    }

    #[tokio::test]
    async fn the_block_write_sets_the_time() {
        with_database("write", |name| async move {
            let mut database = database(&name, false).await;
            database
                .set_block_hashes(10, "h10", "", Some(1_700_000_010))
                .await
                .unwrap();
            database
                .set_block_hashes(11, "h11", "", None)
                .await
                .unwrap();
            assert_eq!(
                times(&database).await,
                vec![(10, Some(1_700_000_010)), (11, None)]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_fill_sets_missing_times_from_the_journal() {
        with_database("fill", |name| async move {
            let database = database(&name, false).await;
            insert_block(&database, 1, None).await;
            insert_block(&database, 2, Some(5)).await;
            insert_block(&database, 3, None).await;
            insert_block(&database, 4, None).await;
            insert_block(&database, 9, None).await;

            // Batches of two, and height 9 is past the journal's tip.
            let filled = database
                .fill_block_times(&Journal { tip: 4 }, 2)
                .await
                .unwrap();
            assert_eq!(filled, 3);
            assert_eq!(
                times(&database).await,
                vec![
                    (1, Some(1001)),
                    (2, Some(5)),
                    (3, Some(1003)),
                    (4, Some(1004)),
                    (9, None)
                ]
            );

            // A second run, as after a restart, finds the rest unfillable.
            let filled = database
                .fill_block_times(&Journal { tip: 4 }, 2)
                .await
                .unwrap();
            assert_eq!(filled, 0);
            let filled = database
                .fill_block_times(&Journal { tip: 9 }, 2)
                .await
                .unwrap();
            assert_eq!(filled, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn light_mode_without_a_time_leaves_null() {
        with_database("light", |name| async move {
            let mut database = database(&name, true).await;
            database.set_block_hashes(7, "h7", "", None).await.unwrap();
            assert_eq!(times(&database).await, vec![(7, None)]);
        })
        .await;
    }
}
