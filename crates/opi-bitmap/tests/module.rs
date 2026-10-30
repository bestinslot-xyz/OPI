use std::collections::BTreeMap;
use std::sync::Arc;

use opi_bitmap::{BitmapConfig, BitmapModule, block_event_hash, cumulative_hash, event_str};
use opi_core::{
    BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord, Module,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

struct Journal;

impl BlockSource for Journal {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Ok(Some(1000))
    }
    fn block_info(&self, _: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(None)
    }
    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }
    fn block_view(&self, _: u32) -> anyhow::Result<Option<BlockView>> {
        Ok(None)
    }
}

struct FailingTip;

impl BlockSource for FailingTip {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        Err(anyhow::anyhow!("journal unavailable"))
    }
    fn block_info(&self, _: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(None)
    }
    fn inscription_records(&self, _: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
        Ok(Vec::new())
    }
    fn block_view(&self, _: u32) -> anyhow::Result<Option<BlockView>> {
        Ok(None)
    }
}

fn view(height: u32, entries: &[(&str, i32, &str)]) -> BlockView {
    let mut records = Vec::new();
    let mut infos = BTreeMap::new();
    for (idx, (id, number, content)) in entries.iter().enumerate() {
        records.push(InscriptionRecord {
            height,
            idx: idx as u32,
            inscription_id: id.to_string(),
            old_satpoint: None,
            new_satpoint: "tx:0:0".into(),
            sent_as_fee: false,
            new_output_value: 546,
            txid: "tx".into(),
            new_pkscript: Vec::new(),
        });
        infos.insert(
            id.to_string(),
            InscriptionInfo {
                inscription_id: id.to_string(),
                inscription_number: *number,
                cursed_for_brc20: false,
                parent_id: None,
                is_json: false,
                content: content.as_bytes().to_vec(),
                content_type: b"text/plain".to_vec(),
                metaprotocol: Vec::new(),
            },
        );
    }
    BlockView {
        height,
        hash: format!("hash{height}"),
        time: 0,
        inscriptions: InscriptionFeed { records, infos },
        runes: opi_core::RuneFeed::default(),
        inscription_moves: Vec::new(),
    }
}

fn admin() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(5432)
        .username("alper")
        .database("postgres")
}

#[tokio::test]
async fn module_round_trip() {
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("opi_bitmap_test_{}_{nanos}", std::process::id());
    let mut root = PgConnection::connect_with(&admin()).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut root)
        .await
        .unwrap();

    // The body runs in its own task so the database is dropped on failure too.
    let result = tokio::spawn(round_trip(name.clone())).await;

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

async fn round_trip(name: String) {
    let name = name.as_str();
    let config = BitmapConfig {
        db_user: "alper".into(),
        db_host: "127.0.0.1".into(),
        db_port: 5432,
        db_database: name.into(),
        db_password: None,
        db_ssl: false,
        network_type: "regtest".into(),
        report_to_indexer: false,
        report_url: String::new(),
        report_retries: 1,
        report_name: "test".into(),
    };
    let mut module = BitmapModule::new(config.clone(), Arc::new(Journal))
        .await
        .unwrap();
    assert_eq!(module.start_height(), 0);
    assert!(module.cursor().await.unwrap().is_none());

    module
        .apply(&view(
            10,
            &[
                ("ai0", 2, "3.bitmap"),
                ("bi0", 1, "3.bitmap"),
                ("ci0", 3, "x"),
            ],
        ))
        .await
        .unwrap();
    module
        .apply(&view(
            11,
            &[("di0", 4, "3.bitmap"), ("ei0", 5, "11.bitmap")],
        ))
        .await
        .unwrap();

    let cursor = module.cursor().await.unwrap().unwrap();
    assert_eq!((cursor.height, cursor.hash.as_str()), (11, "hash11"));
    assert_eq!(
        module.block_hash(10).await.unwrap().as_deref(),
        Some("hash10")
    );

    let mut conn = PgConnection::connect_with(
        &PgConnectOptions::new()
            .host("127.0.0.1")
            .username("alper")
            .database(name),
    )
    .await
    .unwrap();
    let rows =
        sqlx::query("SELECT inscription_id, bitmap_number, block_height FROM bitmaps ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    let rows: Vec<(String, i32, i32)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    assert_eq!(
        rows,
        vec![("bi0".to_string(), 3, 10), ("ei0".to_string(), 11, 11)]
    );

    let first = block_event_hash(&[event_str(3, "bi0")]);
    let second = block_event_hash(&[event_str(11, "ei0")]);
    let hashes = sqlx::query(
        "SELECT block_event_hash, cumulative_event_hash FROM bitmap_cumulative_event_hashes \
         ORDER BY block_height",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(hashes[0].get::<String, _>(0), first);
    assert_eq!(hashes[0].get::<String, _>(1), first);
    assert_eq!(hashes[1].get::<String, _>(0), second);
    assert_eq!(
        hashes[1].get::<String, _>(1),
        cumulative_hash(Some(&first), &second)
    );

    module.rollback_to(10).await.unwrap();
    assert_eq!(module.cursor().await.unwrap().unwrap().height, 10);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM bitmaps")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(left, 1);
    module
        .apply(&view(11, &[("ei0", 5, "11.bitmap")]))
        .await
        .unwrap();
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM bitmaps ORDER BY id")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids, vec![1, 2]);

    let stored: String = sqlx::query_scalar("SELECT indexer_version FROM bitmap_indexer_version")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(stored, opi_bitmap::INDEXER_VERSION);

    // A tip error at report time skips the report; the block stays committed.
    let reporting = BitmapConfig {
        network_type: "signet".into(),
        report_to_indexer: true,
        ..config.clone()
    };
    let mut reporting = BitmapModule::new(reporting, Arc::new(FailingTip))
        .await
        .unwrap();
    reporting
        .apply(&view(12, &[("fi0", 6, "12.bitmap")]))
        .await
        .unwrap();
    assert_eq!(reporting.cursor().await.unwrap().unwrap().height, 12);

    // Pruning drops only the block hashes below its height: the cursor, the
    // bitmaps and the event hashes stay.
    module.prune_below(12).await.unwrap();
    let heights: Vec<i32> = sqlx::query_scalar("SELECT block_height FROM bitmap_block_hashes")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(heights, vec![12]);
    assert_eq!(module.cursor().await.unwrap().unwrap().height, 12);
    for (table, expected) in [("bitmaps", 3), ("bitmap_cumulative_event_hashes", 3)] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(count, expected, "{table}");
    }

    // A second instance on the same database sees the existing version row.
    BitmapModule::new(config, Arc::new(Journal)).await.unwrap();
}
