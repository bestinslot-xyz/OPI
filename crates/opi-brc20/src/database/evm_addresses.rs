//! The `evm_addresses` table: the Bitcoin address of each pkScript a BRC-20
//! event names, beside the EVM address brc20-prog gives that pkScript.
//!
//! The mapping is deterministic, so a reorg leaves the table as it is and a
//! re-indexed block finds its rows already there.

use std::{collections::HashSet, error::Error};

use bitcoin::{Address, Network, Script};
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgConnection, Pool, Postgres, Row};

/// One `evm_addresses` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmAddressRow {
    pub btc_address: String,
    pub evm_address: String,
}

/// The event field holding the pkScript an event of `event_type` is keyed
/// by.
pub fn event_pkscript_field(event_type: i32) -> Option<&'static str> {
    match event_type {
        0 => Some("deployer_pkScript"),
        1 => Some("minted_pkScript"),
        12 => Some("predeployer_pkScript"),
        2 | 4 | 6 | 8 | 10 => Some("source_pkScript"),
        3 | 5 | 7 | 9 | 11 => Some("spent_pkScript"),
        _ => None,
    }
}

/// The address of a P2PKH, P2SH, P2WPKH, P2WSH or P2TR script, in mainnet
/// encoding on mainnet and testnet encoding on every other network. These
/// five are the script types the sidecar's btclib gives an address.
pub fn btc_address(script: &Script, network: Network) -> Option<String> {
    let standard = script.is_p2pkh()
        || script.is_p2sh()
        || script.is_p2wpkh()
        || script.is_p2wsh()
        || script.is_p2tr();
    if !standard {
        return None;
    }
    let network = match network {
        Network::Bitcoin => Network::Bitcoin,
        _ => Network::Testnet,
    };
    Address::from_script(script, network)
        .ok()
        .map(|address| address.to_string())
}

/// brc20-prog's EVM address for a pkScript: "0x" and the lowercase hex of the
/// last 20 bytes of keccak256 over the raw script bytes.
pub fn evm_address(script: &[u8]) -> String {
    let hash = alloy_primitives::keccak256(script);
    format!("0x{}", hex::encode(&hash[12..]))
}

/// The row for a hex pkScript, or `None` when it is not hex or has no
/// address.
pub fn evm_address_row(pkscript_hex: &str, network: Network) -> Option<EvmAddressRow> {
    let script = hex::decode(pkscript_hex).ok()?;
    Some(EvmAddressRow {
        btc_address: btc_address(Script::from_bytes(&script), network)?,
        evm_address: evm_address(&script),
    })
}

/// The rows for `events` (event type and event JSON, in id order), one per
/// Bitcoin address, first occurrence first.
pub fn rows_for_events<'a>(
    events: impl IntoIterator<Item = (i32, &'a Value)>,
    network: Network,
) -> Vec<EvmAddressRow> {
    let mut seen = HashSet::new();
    events
        .into_iter()
        .filter_map(|(event_type, event)| {
            let pkscript = event.get(event_pkscript_field(event_type)?)?.as_str()?;
            evm_address_row(pkscript, network)
        })
        .filter(|row| seen.insert(row.btc_address.clone()))
        .collect()
}

/// Events read per query when filling a new table.
pub const FILL_BATCH: i64 = 10_000;

/// The table opi-api reads.
pub const TABLE: &str = "evm_addresses";

/// The table a fill writes into before it becomes `evm_addresses`.
pub const FILL_TABLE: &str = "evm_addresses_fill";

/// The table as blockscout-setup's extra_sqls.sql defines it. Under the name
/// `evm_addresses` the sequence and constraints get extra_sqls.sql's names.
fn create_table_sql(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (
            id serial PRIMARY KEY,
            evm_address text NOT NULL UNIQUE,
            btc_address text NOT NULL UNIQUE
        )"
    )
}

/// Gives the filled table, its sequence, primary key and unique constraints
/// (with their indexes) the names extra_sqls.sql's DDL produces.
const RENAME_FILL_TABLE: [&str; 6] = [
    "ALTER TABLE evm_addresses_fill RENAME TO evm_addresses",
    "ALTER SEQUENCE evm_addresses_fill_id_seq RENAME TO evm_addresses_id_seq",
    "ALTER TABLE evm_addresses RENAME CONSTRAINT evm_addresses_fill_pkey TO evm_addresses_pkey",
    "ALTER TABLE evm_addresses RENAME CONSTRAINT evm_addresses_fill_evm_address_key TO evm_addresses_evm_address_key",
    "ALTER TABLE evm_addresses RENAME CONSTRAINT evm_addresses_fill_btc_address_key TO evm_addresses_btc_address_key",
    "ALTER TABLE evm_addresses ALTER COLUMN id SET DEFAULT nextval('evm_addresses_id_seq'::regclass)",
];

/// Inserts `rows` into `table` in order. A row whose Bitcoin address the
/// table holds is skipped before it takes an id, so ids stay dense.
pub async fn insert_rows(
    conn: &mut PgConnection,
    table: &str,
    rows: &[EvmAddressRow],
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let (btc_addresses, evm_addresses): (Vec<&str>, Vec<&str>) = rows
        .iter()
        .map(|row| (row.btc_address.as_str(), row.evm_address.as_str()))
        .unzip();
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {table} (btc_address, evm_address)
         SELECT r.btc_address, r.evm_address
         FROM UNNEST($1::text[], $2::text[]) WITH ORDINALITY AS r(btc_address, evm_address, ord)
         WHERE NOT EXISTS (SELECT 1 FROM {table} e WHERE e.btc_address = r.btc_address)
         ORDER BY r.ord
         ON CONFLICT DO NOTHING"
    )))
    .bind(&btc_addresses)
    .bind(&evm_addresses)
    .execute(conn)
    .await?;
    Ok(())
}

/// The sidecar's progress table: one row per block it has written.
const SIDECAR_PROGRESS_TABLE: &str = "brc20_block_hashes_evm_addresses";

/// Makes sure `evm_addresses` exists and is current before indexing.
///
/// An existing table is adopted, less the sidecar's row with an empty
/// `btc_address`. When the sidecar's progress table
/// `brc20_block_hashes_evm_addresses` exists, the events in blocks above its
/// highest block (every event when it is empty) add their rows and the
/// progress table is dropped. A database without the table gets one filled
/// from brc20_events and then brc20_light_events in id order: the rows go
/// into `evm_addresses_fill`, `batch` events per committed transaction, and
/// the finished table is renamed to `evm_addresses`. A fill table left by an
/// interrupted start is dropped and the fill starts over. Returns whether
/// the table was filled.
pub async fn create_and_fill_if_missing(
    pool: &Pool<Postgres>,
    network: Network,
    batch: i64,
) -> Result<bool, Box<dyn Error>> {
    let exists: bool = sqlx::query("SELECT to_regclass('evm_addresses') IS NOT NULL")
        .fetch_one(pool)
        .await?
        .get(0);
    if exists {
        sqlx::query("DELETE FROM evm_addresses WHERE btc_address = ''")
            .execute(pool)
            .await?;
        catch_up_from_sidecar(pool, network, batch).await?;
        return Ok(false);
    }

    tracing::info!("Filling evm_addresses from the indexed events");
    let fill = opi_core::metrics::global().fill("brc20", "evm_addresses");
    sqlx::query(AssertSqlSafe(format!("DROP TABLE IF EXISTS {FILL_TABLE}")))
        .execute(pool)
        .await?;
    sqlx::query(AssertSqlSafe(create_table_sql(FILL_TABLE)))
        .execute(pool)
        .await?;
    let mut read = 0;
    for events_table in ["brc20_events", "brc20_light_events"] {
        read += fill_from(
            pool,
            events_table,
            FILL_TABLE,
            i32::MIN,
            network,
            batch,
            (&fill, read),
        )
        .await?;
    }
    let mut tx = pool.begin().await?;
    for statement in RENAME_FILL_TABLE {
        sqlx::query(statement).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    tracing::info!("Filled evm_addresses");
    Ok(true)
}

/// Adds the rows of the events above the sidecar's last written block to
/// `evm_addresses`, then drops the sidecar's progress table. Does nothing
/// when the progress table is absent.
async fn catch_up_from_sidecar(
    pool: &Pool<Postgres>,
    network: Network,
    batch: i64,
) -> Result<(), Box<dyn Error>> {
    let has_progress: bool = sqlx::query(AssertSqlSafe(format!(
        "SELECT to_regclass('{SIDECAR_PROGRESS_TABLE}') IS NOT NULL"
    )))
    .fetch_one(pool)
    .await?
    .get(0);
    if !has_progress {
        return Ok(());
    }
    let sidecar_height: Option<i32> = sqlx::query(AssertSqlSafe(format!(
        "SELECT max(block_height)::int4 FROM {SIDECAR_PROGRESS_TABLE}"
    )))
    .fetch_one(pool)
    .await?
    .get(0);
    // Block heights are never negative, so -1 selects every event.
    let sidecar_height = sidecar_height.unwrap_or(-1);
    tracing::info!("evm_addresses: adding rows for blocks above {sidecar_height}");
    let fill = opi_core::metrics::global().fill("brc20", "evm_addresses");
    let mut read = 0;
    for events_table in ["brc20_events", "brc20_light_events"] {
        read += fill_from(
            pool,
            events_table,
            TABLE,
            sidecar_height,
            network,
            batch,
            (&fill, read),
        )
        .await?;
    }
    sqlx::query(AssertSqlSafe(format!(
        "DROP TABLE {SIDECAR_PROGRESS_TABLE}"
    )))
    .execute(pool)
    .await?;
    tracing::info!("evm_addresses: dropped {SIDECAR_PROGRESS_TABLE}");
    Ok(())
}

/// A SQL expression for the pkScript of a row's event: the field
/// `event_pkscript_field` names for its `event_type`, NULL for a type
/// without one.
fn pkscript_sql() -> String {
    let mut cases = String::new();
    for event_type in 0..=12 {
        if let Some(field) = event_pkscript_field(event_type) {
            cases.push_str(&format!(" WHEN {event_type} THEN event->>'{field}'"));
        }
    }
    format!("CASE event_type{cases} END")
}

/// Inserts into `target` the rows of the events of `events_table` in blocks
/// above `above_height`, committing each batch, and returns the events read.
/// Logs the events read per source table. `progress` is the fill's metrics
/// and the events its earlier tables read; each batch sets the fill's
/// progress to the events read in all.
async fn fill_from(
    pool: &Pool<Postgres>,
    events_table: &str,
    target: &str,
    above_height: i32,
    network: Network,
    batch: i64,
    progress: (&opi_core::metrics::FillGuard, usize),
) -> Result<usize, Box<dyn Error>> {
    let query = format!(
        "SELECT id, {} AS pkscript FROM {events_table}
         WHERE id > $1 AND block_height > $2 ORDER BY id LIMIT $3",
        pkscript_sql()
    );
    let mut after_id = i64::MIN;
    let mut read = 0usize;
    loop {
        let events = sqlx::query(AssertSqlSafe(query.as_str()))
            .bind(after_id)
            .bind(above_height)
            .bind(batch)
            .fetch_all(pool)
            .await?;
        let Some(last) = events.last() else {
            break;
        };
        after_id = last.get("id");
        let mut seen = HashSet::new();
        let rows: Vec<EvmAddressRow> = events
            .iter()
            .filter_map(|event| {
                let pkscript: Option<String> = event.get("pkscript");
                evm_address_row(&pkscript?, network)
            })
            .filter(|row| seen.insert(row.btc_address.clone()))
            .collect();
        let mut conn = pool.acquire().await?;
        insert_rows(&mut conn, target, &rows).await?;
        read += events.len();
        let (fill, before) = progress;
        fill.progress(i64::try_from(before + read).unwrap_or(i64::MAX));
        if read % 1_000_000 < events.len() {
            tracing::info!("evm_addresses: read {read} events from {events_table}");
        }
    }
    tracing::info!("evm_addresses: read {read} events from {events_table} in total");
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Produced by running the blockscout-setup sidecar's own
    /// `get_btc_address_from_pkscript` and `get_evm_address`
    /// (brc20_update_evm_addresses_custom.py, btclib 2023.7.12) on each
    /// script, with NETWORK_TYPE mainnet and signet.
    const GOLDEN: [(&str, &str, &str, &str); 5] = [
        (
            "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            "mpXwg4jMtRhuSpVq4xS3HFHmCmWp9NyGKt",
            "0xa2044bb0be101265ce877d9ad204215ede06ac64",
        ),
        (
            "a914748284390f9e263a4b766a75d0633c50426eb87587",
            "3CK4fEwbMP7heJarmU4eqA3sMbVJyEnU3V",
            "2N3sGiyscxqd3r6DQSbgXT738ZwhUpBqkej",
            "0xbd75f65355b8f844ac0f55302a328e92f8417028",
        ),
        (
            "0014751e76e8199196d454941c45d1b3a323f1433bd6",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            "0x322061d016f20128abcb17bd41fa76e4665d96a3",
        ),
        (
            "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262",
            "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7",
            "0xce747745f9851a814151dae428c0e5414599407d",
        ),
        (
            "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c",
            "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
            "tb1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqp3mvzv",
            "0xc63da1b33495397e478732178bb4ab151f87db36",
        ),
    ];

    const P2PK: &str = "2103a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bdac";
    const NULLDATA: &str = "6a0b68656c6c6f20776f726c64";
    const P2MS: &str = "5121022afc20bf379bc96a2f4e9e63ffceb8652b2b6a097f63fbee6ecec2a49a48010e2103a767c7221e9f15f870f1ad9311f5ab937d79fcaeee15bb2c722bca515581b4c052ae";
    /// Witness programs the sidecar's btclib gives no address.
    const WITNESS_V2: &str = "5210751e76e8199196d454941c45d1b3a323";
    const WITNESS_V1_20_BYTES: &str = "5114751e76e8199196d454941c45d1b3a323f1433bd6";

    fn row(btc_address: &str, evm_address: &str) -> EvmAddressRow {
        EvmAddressRow {
            btc_address: btc_address.into(),
            evm_address: evm_address.into(),
        }
    }

    #[test]
    fn standard_scripts_match_the_sidecar_on_mainnet_and_signet() {
        for (script, mainnet, testnet, evm) in GOLDEN {
            assert_eq!(
                evm_address_row(script, Network::Bitcoin),
                Some(row(mainnet, evm)),
                "{script}"
            );
            for network in [Network::Signet, Network::Testnet, Network::Regtest] {
                assert_eq!(
                    evm_address_row(script, network),
                    Some(row(testnet, evm)),
                    "{script} on {network}"
                );
            }
        }
    }

    #[test]
    fn evm_address_is_brc20_progs_pkscript_mapping() {
        for (script, _, _, evm) in GOLDEN {
            assert_eq!(evm_address(&hex::decode(script).unwrap()), evm);
        }
    }

    #[test]
    fn scripts_without_a_standard_address_are_skipped() {
        for script in [
            P2PK,
            NULLDATA,
            P2MS,
            WITNESS_V2,
            WITNESS_V1_20_BYTES,
            "51",
            "",
            "zz",
        ] {
            assert_eq!(evm_address_row(script, Network::Bitcoin), None, "{script}");
            assert_eq!(evm_address_row(script, Network::Signet), None, "{script}");
        }
    }

    #[test]
    fn each_event_type_names_its_pkscript_field() {
        assert_eq!(event_pkscript_field(0), Some("deployer_pkScript"));
        assert_eq!(event_pkscript_field(1), Some("minted_pkScript"));
        assert_eq!(event_pkscript_field(12), Some("predeployer_pkScript"));
        for event_type in [2, 4, 6, 8, 10] {
            assert_eq!(event_pkscript_field(event_type), Some("source_pkScript"));
        }
        for event_type in [3, 5, 7, 9, 11] {
            assert_eq!(event_pkscript_field(event_type), Some("spent_pkScript"));
        }
        assert_eq!(event_pkscript_field(13), None);
        assert_eq!(event_pkscript_field(-1), None);
    }

    #[test]
    fn block_rows_follow_event_order_once_per_address() {
        let (p2pkh, p2pkh_main, _, p2pkh_evm) = GOLDEN[0];
        let (p2tr, p2tr_main, _, p2tr_evm) = GOLDEN[4];
        let (p2wpkh, p2wpkh_main, _, p2wpkh_evm) = GOLDEN[2];
        let events = [
            (0, json!({ "deployer_pkScript": p2tr })),
            (1, json!({ "minted_pkScript": NULLDATA })),
            // A transfer reads the source, not the spent, pkScript.
            (
                2,
                json!({ "source_pkScript": p2pkh, "spent_pkScript": p2wpkh }),
            ),
            (
                3,
                json!({ "source_pkScript": p2pkh, "spent_pkScript": p2wpkh }),
            ),
            (1, json!({ "minted_pkScript": p2tr })),
            // An event without its field is skipped.
            (12, json!({ "source_pkScript": p2pkh })),
        ];
        assert_eq!(
            rows_for_events(events.iter().map(|(t, e)| (*t, e)), Network::Bitcoin),
            vec![
                row(p2tr_main, p2tr_evm),
                row(p2pkh_main, p2pkh_evm),
                row(p2wpkh_main, p2wpkh_evm),
            ]
        );
    }
}

/// Postgres-backed tests, each on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use serde_json::json;
    use sqlx::{Connection, PgConnection, Row, postgres::PgConnectOptions};

    use super::*;
    use crate::{
        config::Brc20IndexerConfig,
        database::{
            Brc20Database,
            brc20_database::{EventRecord, LightEventRecord},
        },
    };

    const P2PKH: &str = "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac";
    const P2PKH_ADDR: &str = "mpXwg4jMtRhuSpVq4xS3HFHmCmWp9NyGKt";
    const P2PKH_EVM: &str = "0xa2044bb0be101265ce877d9ad204215ede06ac64";
    const P2SH: &str = "a914748284390f9e263a4b766a75d0633c50426eb87587";
    const P2SH_ADDR: &str = "2N3sGiyscxqd3r6DQSbgXT738ZwhUpBqkej";
    const P2SH_EVM: &str = "0xbd75f65355b8f844ac0f55302a328e92f8417028";
    const P2WPKH: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
    const P2WPKH_ADDR: &str = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx";
    const P2WPKH_EVM: &str = "0x322061d016f20128abcb17bd41fa76e4665d96a3";
    const P2TR: &str = "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c";
    const P2TR_ADDR: &str = "tb1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqp3mvzv";
    const P2TR_EVM: &str = "0xc63da1b33495397e478732178bb4ab151f87db36";
    const NULLDATA: &str = "6a0b68656c6c6f20776f726c64";
    const NULLDATA_EVM: &str = "0x26b86118b28b9037b8a5e61c2766fd8312d477ac";

    /// The sidecar's DDL (blockscout-setup extra_sqls.sql) without its
    /// OWNER statements.
    const SIDECAR_DDL: &str = "
        CREATE TABLE public.evm_addresses (
            id integer NOT NULL,
            evm_address text NOT NULL,
            btc_address text NOT NULL
        );
        CREATE SEQUENCE public.evm_addresses_id_seq AS integer START WITH 1
            INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1;
        ALTER SEQUENCE public.evm_addresses_id_seq OWNED BY public.evm_addresses.id;
        ALTER TABLE ONLY public.evm_addresses ALTER COLUMN id
            SET DEFAULT nextval('public.evm_addresses_id_seq'::regclass);
        ALTER TABLE ONLY public.evm_addresses
            ADD CONSTRAINT evm_addresses_btc_address_key UNIQUE (btc_address);
        ALTER TABLE ONLY public.evm_addresses
            ADD CONSTRAINT evm_addresses_evm_address_key UNIQUE (evm_address);
        ALTER TABLE ONLY public.evm_addresses
            ADD CONSTRAINT evm_addresses_pkey PRIMARY KEY (id);";

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
        let name = format!("opi_brc20_evm_{tag}_{}_{nanos}", std::process::id());
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

    async fn database(name: &str, light_client_mode: bool) -> Brc20Database {
        let config = Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: Network::Signet,
            light_client_mode,
            ..Default::default()
        };
        let mut database = Brc20Database::new(&config);
        database.init().await.unwrap();
        database
    }

    fn push_event(database: &mut Brc20Database, block_height: i32, event_type: i32, event: Value) {
        let event_id = database.current_event_id;
        database.event_inserts.push(EventRecord {
            event_id,
            event_type_id: event_type,
            block_height,
            inscription_id: format!("{event_id}i0"),
            inscription_number: event_id as i32,
            old_satpoint: None,
            new_satpoint: "sat:0:0".into(),
            txid: "tx".into(),
            event,
        });
        database.current_event_id += 1;
    }

    fn push_light_event(
        database: &mut Brc20Database,
        block_height: i32,
        event_type: i32,
        event: Value,
    ) {
        let event_id = database.current_event_id;
        database.light_event_inserts.push(LightEventRecord {
            event_id,
            event_type_id: event_type,
            block_height,
            inscription_id: format!("{event_id}i0"),
            event,
        });
        database.current_event_id += 1;
    }

    async fn table(database: &Brc20Database) -> Vec<(i32, String, String)> {
        sqlx::query("SELECT id, btc_address, evm_address FROM evm_addresses ORDER BY id")
            .fetch_all(&database.client)
            .await
            .unwrap()
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect()
    }

    fn expected(rows: &[(&str, &str)]) -> Vec<(i32, String, String)> {
        rows.iter()
            .enumerate()
            .map(|(i, (btc, evm))| (i as i32 + 1, btc.to_string(), evm.to_string()))
            .collect()
    }

    /// Two blocks: a deploy, mints (one to an OP_RETURN, one repeating an
    /// address) and a transfer.
    fn index_two_blocks(database: &mut Brc20Database) {
        push_event(database, 200, 0, json!({ "deployer_pkScript": P2TR }));
        push_event(database, 200, 1, json!({ "minted_pkScript": NULLDATA }));
        push_event(database, 200, 1, json!({ "minted_pkScript": P2WPKH }));
        push_event(database, 200, 1, json!({ "minted_pkScript": P2TR }));
        push_event(
            database,
            201,
            3,
            json!({ "source_pkScript": P2WPKH, "spent_pkScript": P2PKH }),
        );
    }

    #[tokio::test]
    async fn a_new_database_gets_an_empty_table() {
        with_database("new", |name| async move {
            let database = database(&name, false).await;
            assert_eq!(table(&database).await, Vec::new());
        })
        .await;
    }

    #[tokio::test]
    async fn each_block_writes_its_rows_and_a_reorg_keeps_them() {
        with_database("blocks", |name| async move {
            let mut database = database(&name, false).await;
            index_two_blocks(&mut database);
            database.flush_queries_to_db().await.unwrap();
            let indexed = expected(&[
                (P2TR_ADDR, P2TR_EVM),
                (P2WPKH_ADDR, P2WPKH_EVM),
                (P2PKH_ADDR, P2PKH_EVM),
            ]);
            assert_eq!(table(&database).await, indexed);

            // The reorg removes block 201's events and leaves the table.
            database.reorg(200).await.unwrap();
            assert_eq!(table(&database).await, indexed);

            // Re-indexing 201 adds only the new address, with the next id.
            push_event(
                &mut database,
                201,
                3,
                json!({ "source_pkScript": P2WPKH, "spent_pkScript": P2PKH }),
            );
            push_event(&mut database, 201, 1, json!({ "minted_pkScript": P2SH }));
            database.flush_queries_to_db().await.unwrap();
            assert_eq!(
                table(&database).await,
                expected(&[
                    (P2TR_ADDR, P2TR_EVM),
                    (P2WPKH_ADDR, P2WPKH_EVM),
                    (P2PKH_ADDR, P2PKH_EVM),
                    (P2SH_ADDR, P2SH_EVM),
                ])
            );
        })
        .await;
    }

    #[tokio::test]
    async fn light_mode_blocks_write_their_rows() {
        with_database("light", |name| async move {
            let mut database = database(&name, true).await;
            push_light_event(&mut database, 200, 0, json!({ "deployer_pkScript": P2TR }));
            push_light_event(
                &mut database,
                200,
                1,
                json!({ "minted_pkScript": NULLDATA }),
            );
            push_light_event(&mut database, 200, 2, json!({ "source_pkScript": P2PKH }));
            database.flush_queries_to_db().await.unwrap();
            assert_eq!(
                table(&database).await,
                expected(&[(P2TR_ADDR, P2TR_EVM), (P2PKH_ADDR, P2PKH_EVM)])
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_database_without_the_table_is_filled_at_init() {
        with_database("fill", |name| async move {
            let mut full = database(&name, false).await;
            index_two_blocks(&mut full);
            full.flush_queries_to_db().await.unwrap();
            let mut light = database(&name, true).await;
            light.current_event_id = 100;
            push_light_event(&mut light, 202, 1, json!({ "minted_pkScript": P2SH }));
            push_light_event(&mut light, 202, 1, json!({ "minted_pkScript": P2TR }));
            light.flush_queries_to_db().await.unwrap();
            let indexed = table(&full).await;
            assert_eq!(indexed.len(), 4);

            sqlx::query("DROP TABLE evm_addresses")
                .execute(&full.client)
                .await
                .unwrap();
            let restarted = database(&name, false).await;
            assert_eq!(table(&restarted).await, indexed);

            // One event per query fills the same rows.
            sqlx::query("DROP TABLE evm_addresses")
                .execute(&full.client)
                .await
                .unwrap();
            assert!(
                create_and_fill_if_missing(&full.client, Network::Signet, 1)
                    .await
                    .unwrap()
            );
            assert_eq!(table(&full).await, indexed);
        })
        .await;
    }

    #[tokio::test]
    async fn an_existing_table_is_adopted_and_not_refilled() {
        with_database("adopt", |name| async move {
            let mut first = database(&name, false).await;
            index_two_blocks(&mut first);
            first.flush_queries_to_db().await.unwrap();
            sqlx::query("DROP TABLE evm_addresses")
                .execute(&first.client)
                .await
                .unwrap();
            sqlx::raw_sql(SIDECAR_DDL)
                .execute(&first.client)
                .await
                .unwrap();
            // The sidecar stores its first script without an address under
            // an empty btc_address.
            for (btc, evm) in [(P2SH_ADDR, P2SH_EVM), ("", NULLDATA_EVM)] {
                sqlx::query("INSERT INTO evm_addresses (btc_address, evm_address) VALUES ($1, $2)")
                    .bind(btc)
                    .bind(evm)
                    .execute(&first.client)
                    .await
                    .unwrap();
            }

            let mut adopted = database(&name, false).await;
            assert_eq!(table(&adopted).await, expected(&[(P2SH_ADDR, P2SH_EVM)]));

            push_event(&mut adopted, 202, 1, json!({ "minted_pkScript": P2SH }));
            push_event(&mut adopted, 202, 1, json!({ "minted_pkScript": P2TR }));
            adopted.flush_queries_to_db().await.unwrap();
            assert_eq!(
                table(&adopted).await,
                vec![
                    (1, P2SH_ADDR.to_string(), P2SH_EVM.to_string()),
                    (3, P2TR_ADDR.to_string(), P2TR_EVM.to_string()),
                ]
            );
        })
        .await;
    }

    /// The sidecar's progress table, with its blocks at `heights`.
    async fn sidecar_progress(database: &Brc20Database, heights: &[i32]) {
        sqlx::raw_sql(
            "CREATE TABLE brc20_block_hashes_evm_addresses (
                id serial PRIMARY KEY, block_height int4 NOT NULL, block_hash text NOT NULL)",
        )
        .execute(&database.client)
        .await
        .unwrap();
        for height in heights {
            sqlx::query(
                "INSERT INTO brc20_block_hashes_evm_addresses (block_height, block_hash) VALUES ($1, 'h')",
            )
            .bind(height)
            .execute(&database.client)
            .await
            .unwrap();
        }
    }

    async fn progress_table_gone(database: &Brc20Database) -> bool {
        sqlx::query("SELECT to_regclass('brc20_block_hashes_evm_addresses') IS NULL")
            .fetch_one(&database.client)
            .await
            .unwrap()
            .get(0)
    }

    /// Swaps the table for the sidecar's, holding `rows`.
    async fn swap_in_sidecar_table(database: &Brc20Database, rows: &[(&str, &str)]) {
        sqlx::query("DROP TABLE evm_addresses")
            .execute(&database.client)
            .await
            .unwrap();
        sqlx::raw_sql(SIDECAR_DDL)
            .execute(&database.client)
            .await
            .unwrap();
        for (btc, evm) in rows {
            sqlx::query("INSERT INTO evm_addresses (btc_address, evm_address) VALUES ($1, $2)")
                .bind(btc)
                .bind(evm)
                .execute(&database.client)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn adoption_catches_up_from_the_sidecars_progress_table() {
        with_database("catchup", |name| async move {
            let mut first = database(&name, false).await;
            index_two_blocks(&mut first);
            first.flush_queries_to_db().await.unwrap();
            let mut light = database(&name, true).await;
            light.current_event_id = 100;
            push_light_event(&mut light, 202, 1, json!({ "minted_pkScript": P2SH }));
            light.flush_queries_to_db().await.unwrap();

            // The sidecar wrote block 200 only.
            swap_in_sidecar_table(&first, &[(P2TR_ADDR, P2TR_EVM), (P2WPKH_ADDR, P2WPKH_EVM)])
                .await;
            sidecar_progress(&first, &[199, 200]).await;

            let adopted = database(&name, false).await;
            assert_eq!(
                table(&adopted).await,
                expected(&[
                    (P2TR_ADDR, P2TR_EVM),
                    (P2WPKH_ADDR, P2WPKH_EVM),
                    (P2PKH_ADDR, P2PKH_EVM),
                    (P2SH_ADDR, P2SH_EVM),
                ])
            );
            assert!(progress_table_gone(&adopted).await);

            // A second start has nothing more to do.
            let before = table(&adopted).await;
            let again = database(&name, false).await;
            assert_eq!(table(&again).await, before);
        })
        .await;
    }

    #[tokio::test]
    async fn an_empty_progress_table_catches_up_every_event() {
        with_database("emptyprogress", |name| async move {
            let mut first = database(&name, false).await;
            index_two_blocks(&mut first);
            first.flush_queries_to_db().await.unwrap();
            let indexed = table(&first).await;
            swap_in_sidecar_table(&first, &[]).await;
            sidecar_progress(&first, &[]).await;

            let adopted = database(&name, false).await;
            assert_eq!(table(&adopted).await, indexed);
            assert!(progress_table_gone(&adopted).await);
        })
        .await;
    }

    #[tokio::test]
    async fn reset_drops_the_table_and_a_fresh_init_recreates_it_empty() {
        with_database("reset", |name| async move {
            let mut first = database(&name, false).await;
            index_two_blocks(&mut first);
            first.flush_queries_to_db().await.unwrap();
            assert_eq!(table(&first).await.len(), 3);
            first.reset().await.unwrap();
            let fresh = database(&name, false).await;
            assert_eq!(table(&fresh).await, Vec::new());
        })
        .await;
    }

    /// The table's columns, constraints, indexes and id sequence, by name.
    async fn shape(database: &Brc20Database) -> Vec<String> {
        let queries = [
            "SELECT column_name || ' ' || data_type || ' ' || is_nullable || ' ' || coalesce(column_default, '')
             FROM information_schema.columns WHERE table_name = 'evm_addresses'",
            "SELECT conname || ' ' || contype || ' ' || pg_get_constraintdef(oid)
             FROM pg_constraint WHERE conrelid = 'evm_addresses'::regclass",
            "SELECT indexname || ' ' || indexdef FROM pg_indexes WHERE tablename = 'evm_addresses'",
            "SELECT pg_get_serial_sequence('evm_addresses', 'id')",
            "SELECT sequencename || ' ' || data_type::text FROM pg_sequences
             WHERE sequencename LIKE 'evm_addresses%'",
        ];
        let mut shape = Vec::new();
        for query in queries {
            let mut rows: Vec<String> = sqlx::query(query)
                .fetch_all(&database.client)
                .await
                .unwrap()
                .iter()
                .map(|row| row.get(0))
                .collect();
            rows.sort();
            shape.extend(rows);
        }
        shape
    }

    #[tokio::test]
    async fn a_filled_table_has_the_sidecar_tables_names() {
        with_database("shape", |name| async move {
            let database = database(&name, false).await;
            sqlx::query("DROP TABLE evm_addresses")
                .execute(&database.client)
                .await
                .unwrap();
            assert!(
                create_and_fill_if_missing(&database.client, Network::Signet, FILL_BATCH)
                    .await
                    .unwrap()
            );
            let filled = shape(&database).await;

            sqlx::query("DROP TABLE evm_addresses")
                .execute(&database.client)
                .await
                .unwrap();
            sqlx::raw_sql(SIDECAR_DDL)
                .execute(&database.client)
                .await
                .unwrap();
            assert_eq!(filled, shape(&database).await);
            assert!(
                filled
                    .iter()
                    .any(|line| line.contains("evm_addresses_id_seq"))
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_fill_table_left_by_an_interrupted_start_is_dropped() {
        with_database("restart", |name| async move {
            let mut full = database(&name, false).await;
            index_two_blocks(&mut full);
            full.flush_queries_to_db().await.unwrap();
            let indexed = table(&full).await;

            sqlx::raw_sql(AssertSqlSafe(format!(
                "DROP TABLE evm_addresses;
                 {};
                 INSERT INTO evm_addresses_fill (btc_address, evm_address) VALUES ('junk', '0xjunk')",
                create_table_sql(FILL_TABLE)
            )))
            .execute(&full.client)
            .await
            .unwrap();

            let restarted = database(&name, false).await;
            assert_eq!(table(&restarted).await, indexed);
            let fill_table_gone: bool =
                sqlx::query("SELECT to_regclass('evm_addresses_fill') IS NULL")
                    .fetch_one(&restarted.client)
                    .await
                    .unwrap()
                    .get(0);
            assert!(fill_table_gone);
        })
        .await;
    }
}
