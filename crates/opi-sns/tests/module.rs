use std::collections::BTreeMap;
use std::sync::Arc;

use opi_core::{
    BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord, Module,
};
use opi_sns::{
    SnsConfig, SnsModule, block_event_hash, cumulative_hash, ns_register_event_str,
    register_event_str,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

/// A journal whose tip is 1000, or whose tip read fails.
struct Journal {
    tip_fails: bool,
}

impl BlockSource for Journal {
    fn tip(&self) -> anyhow::Result<Option<u32>> {
        if self.tip_fails {
            anyhow::bail!("journal tip unavailable");
        }
        Ok(Some(1000))
    }
    fn block_info(&self, _: u32) -> anyhow::Result<Option<BlockInfo>> {
        Ok(None)
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
                content_type: b"text/plain;charset=utf-8".to_vec(),
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
    }
}

fn admin() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(5432)
        .username("alper")
        .database("postgres")
}

/// Runs `body` against a throwaway database, dropped even when `body`
/// panics. Skips unless `OPI_TEST_PG=1`.
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
    let name = format!("opi_sns_test_{tag}_{}_{nanos}", std::process::id());
    let mut root = PgConnection::connect_with(&admin()).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut root)
        .await
        .unwrap();

    // The body runs in its own task so the database is dropped on failure too.
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
async fn module_round_trip() {
    with_database("round_trip", round_trip).await;
}

/// A database indexed under the old rules (db_version 4) is refused, which
/// forces a re-index.
#[tokio::test]
async fn version_gate_requires_5() {
    with_database("version_gate", |name| async move {
        let config = config(&name);
        SnsModule::new(config.clone(), Arc::new(Journal { tip_fails: false }))
            .await
            .unwrap();
        let mut conn = PgConnection::connect_with(&admin().database(&name))
            .await
            .unwrap();
        let seeded: (String, i32) =
            sqlx::query_as("SELECT indexer_version, db_version FROM sns_names_indexer_version")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(seeded, ("opi-sns-names-full-node v3.0.0".into(), 5));
        sqlx::query("UPDATE sns_names_indexer_version SET db_version = 4")
            .execute(&mut conn)
            .await
            .unwrap();
        let error = SnsModule::new(config, Arc::new(Journal { tip_fails: false }))
            .await
            .err()
            .expect("db_version 4 is refused")
            .to_string();
        assert!(error.contains("db_version 4 is not 5"), "{error}");
    })
    .await;
}

fn config(name: &str) -> SnsConfig {
    SnsConfig {
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
    }
}

const NS: &str = r#"{"p":"sns","op":"ns","ns":"Sats"}"#;

async fn round_trip(name: String) {
    let name = name.as_str();
    let config = SnsConfig {
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
    let mut module = SnsModule::new(config.clone(), Arc::new(Journal { tip_fails: false }))
        .await
        .unwrap();
    assert_eq!(module.start_height(), 0);
    assert!(module.cursor().await.unwrap().is_none());

    module
        .apply(&view(
            10,
            &[
                ("ai0", 2, "Alice.sats"),
                ("bi0", 1, "alice.SATS"),
                ("ci0", 3, "nothing"),
                ("di0", 4, NS),
            ],
        ))
        .await
        .unwrap();
    module
        .apply(&view(
            11,
            &[
                ("ei0", 5, "alice.sats"),
                ("fi0", 6, "bob.sats"),
                ("gi0", 7, NS),
            ],
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
    let rows = sqlx::query(
        "SELECT inscription_id, inscription_number, \"name\", domain, block_height \
         FROM sns_names ORDER BY id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    let rows: Vec<(String, i32, String, String, i32)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("bi0".into(), 1, "alice.sats".into(), "sats".into(), 10),
            ("fi0".into(), 6, "bob.sats".into(), "sats".into(), 11),
        ]
    );
    let namespaces: Vec<(String, String, i32)> = sqlx::query(
        "SELECT inscription_id, \"namespace\", block_height FROM sns_namespaces ORDER BY id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2)))
    .collect();
    assert_eq!(namespaces, vec![("di0".into(), "sats".into(), 10)]);

    let first = block_event_hash(&[
        register_event_str("alice.sats", "sats", "bi0"),
        ns_register_event_str("sats", "di0"),
    ]);
    let second = block_event_hash(&[register_event_str("bob.sats", "sats", "fi0")]);
    let hashes = sqlx::query(
        "SELECT block_event_hash, cumulative_event_hash FROM sns_names_cumulative_event_hashes \
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
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sns_names")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(left, 1);
    module
        .apply(&view(11, &[("fi0", 6, "bob.sats")]))
        .await
        .unwrap();
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM sns_names ORDER BY id")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids, vec![1, 2]);

    // Rows above the cursor are residue of an interrupted run; a new
    // instance drops them.
    sqlx::query(
        "INSERT INTO sns_namespaces (inscription_id, inscription_number, \"namespace\", \
         block_height) VALUES ('zi0', 9, 'late', 12)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    SnsModule::new(config.clone(), Arc::new(Journal { tip_fails: false }))
        .await
        .unwrap();
    let late: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sns_namespaces WHERE block_height > 11")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(late, 0);

    // A report that cannot read the journal tip is skipped; the block stays
    // applied.
    let reporting = SnsConfig {
        network_type: "signet".into(),
        report_to_indexer: true,
        ..config
    };
    let mut module = SnsModule::new(reporting, Arc::new(Journal { tip_fails: true }))
        .await
        .unwrap();
    module.apply(&view(12, &[])).await.unwrap();
    assert_eq!(module.cursor().await.unwrap().unwrap().height, 12);
}
