use core::panic;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
    vec,
};

use axum::body::Bytes;
use bitcoin::Network;
use lazy_static::lazy_static;
use num_traits::ToPrimitive;
use once_cell::sync::OnceCell;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Pool, Postgres, Row, postgres::PgPoolOptions, types::BigDecimal};
use tokio::sync::Mutex;

use crate::{
    config::{
        Brc20IndexerConfig, EVENT_HASH_VERSION, EVENT_SEPARATOR, INDEXER_VERSION,
        LIGHT_CLIENT_VERSION,
    },
    types::{
        Ticker,
        events::{Event, MintInscribeEvent, load_event_str},
    },
};

lazy_static! {
    static ref ALL_TABLES_WITH_BLOCK_HEIGHT: Vec<&'static str> = vec![
        "brc20_tickers",
        "brc20_historic_balances",
        "brc20_events",
        "brc20_light_events",
        "brc20_cumulative_event_hashes",
        "brc20_bitcoin_rpc_result_cache",
        "brc20_block_hashes",
        "brc20_unused_txes",
        "brc20_current_balances",
        "brc20_logs",
    ];
}

#[derive(Embed)]
#[folder = "src/database/sql"]
pub struct SqlFiles;

impl SqlFiles {
    pub fn get_sql_file(file_name: &str) -> Option<String> {
        let file = SqlFiles::get(file_name)?;
        let sql = String::from_utf8_lossy(&file.data);
        Some(sql.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum TransferValidity {
    Valid,
    Invalid,
    Used,
}

#[derive(Debug, Clone)]
pub struct Brc20Balance {
    pub overall_balance: u128,
    pub available_balance: u128,
}

#[derive(Debug, Clone)]
pub struct TickerUpdateData {
    pub remaining_supply: u128,
    pub burned_supply: u128,
}

#[derive(Debug, Clone)]
pub struct LightEventRecord {
    pub event_id: i64,
    pub event_type_id: i32,
    pub block_height: i32,
    pub inscription_id: String,
    pub event: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event_id: i64,
    pub event_type_id: i32,
    pub block_height: i32,
    pub inscription_id: String,
    pub inscription_number: i32,
    pub old_satpoint: Option<String>,
    pub new_satpoint: String,
    pub txid: String,
    pub event: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct BalanceUpdateData {
    pub ticker: String,
    pub pkscript: String,
    pub wallet: String,
    pub overall_balance: u128,
    pub available_balance: u128,
    pub block_height: i32,
    pub event_id: i64,
}

#[derive(Debug, Clone)]
pub struct BitcoinRpcResultRecord {
    pub method: String,
    pub response: serde_json::Value,
    pub block_height: i32,
}

pub static BRC20_DATABASE: OnceCell<Arc<Mutex<Brc20Database>>> = OnceCell::new();

pub fn get_brc20_database() -> Arc<Mutex<Brc20Database>> {
    BRC20_DATABASE
        .get()
        .cloned()
        .expect("Brc20Database not initialized")
}

pub fn set_brc20_database(database: Arc<Mutex<Brc20Database>>) {
    BRC20_DATABASE
        .set(database)
        .expect("Brc20Database already initialized");
}

#[derive(Debug)]
pub struct Brc20Database {
    pub client: Pool<Postgres>,
    pub bitcoin_rpc_cache_enabled: bool,
    pub network: Network,
    pub first_inscription_height: i32,
    pub transfer_validity_cache: HashMap<String, TransferValidity>,
    pub current_event_id: i64,
    pub tickers: HashMap<String, Ticker>,
    pub cached_events: HashMap<String, serde_json::Value>,
    pub balance_cache: HashMap<String, Brc20Balance>,

    pub new_tickers: Vec<Ticker>,
    pub ticker_updates: HashMap<String, TickerUpdateData>,
    pub light_event_inserts: Vec<LightEventRecord>,
    pub event_inserts: Vec<EventRecord>,
    pub bitcoin_rpc_inserts: HashMap<serde_json::Value, BitcoinRpcResultRecord>,
    pub log_timer_inserts: HashMap<i32, HashMap<String, Vec<u128>>>,
    pub block_event_strings: HashMap<i32, String>,
    pub balance_updates: Vec<BalanceUpdateData>,
    pub light_client_mode: bool,
    pub save_logs: bool,
    pub events_table: String,
}

impl Brc20Database {
    pub fn new(config: &Brc20IndexerConfig) -> Self {
        let ssl_mode = if config.db_ssl {
            "?sslmode=require"
        } else {
            ""
        };
        tracing::info!(
            "Connecting to database at {}",
            &format!(
                "postgres://{}:{}@{}:{}/{}{}",
                config
                    .db_user
                    .replace("/", "%2F")
                    .replace(":", "%3A")
                    .replace("@", "%40"),
                "**********",
                config.db_host,
                config.db_port,
                config
                    .db_database
                    .replace("/", "%2F")
                    .replace(":", "%3A")
                    .replace("@", "%40"),
                ssl_mode
            )
        );
        let client = PgPoolOptions::new()
            .max_connections(5)
            .acquire_slow_threshold(Duration::from_secs(10))
            .connect_lazy(&format!(
                "postgres://{}:{}@{}:{}/{}{}",
                config
                    .db_user
                    .replace("/", "%2F")
                    .replace(":", "%3A")
                    .replace("@", "%40"),
                config
                    .db_password
                    .replace("/", "%2F")
                    .replace(":", "%3A")
                    .replace("@", "%40"),
                config.db_host,
                config.db_port,
                config
                    .db_database
                    .replace("/", "%2F")
                    .replace(":", "%3A")
                    .replace("@", "%40"),
                ssl_mode
            ))
            .expect("Failed to connect to the database");
        Brc20Database {
            client,
            network: config.network_type,
            first_inscription_height: config.first_inscription_height,
            transfer_validity_cache: HashMap::new(),
            current_event_id: 0,
            bitcoin_rpc_cache_enabled: config.bitcoin_rpc_cache_enabled,
            tickers: HashMap::new(),
            cached_events: HashMap::new(),
            balance_cache: HashMap::new(),

            new_tickers: Vec::new(),
            ticker_updates: HashMap::new(),
            event_inserts: Vec::new(),
            log_timer_inserts: HashMap::new(),
            light_event_inserts: Vec::new(),
            bitcoin_rpc_inserts: HashMap::new(),
            balance_updates: Vec::new(),
            block_event_strings: HashMap::new(),
            light_client_mode: config.light_client_mode,
            save_logs: config.save_logs,
            events_table: if config.light_client_mode {
                "brc20_light_events".to_string()
            } else {
                "brc20_events".to_string()
            },
        }
    }

    pub async fn init(&mut self) -> Result<(), Box<dyn Error>> {
        if let Err(sqlx::Error::RowNotFound) =
            sqlx::query!("SELECT * FROM pg_tables WHERE tablename = 'brc20_block_hashes' LIMIT 1")
                .fetch_one(&self.client)
                .await
        {
            sqlx::raw_sql(&SqlFiles::get_sql_file("db_init.sql").unwrap())
                .execute(&self.client)
                .await?;
        };

        self.fetch_current_event_id().await?;

        let start_time = Instant::now();
        for ticker in self.get_tickers().await? {
            self.tickers.insert(ticker.ticker.clone(), ticker);
        }
        tracing::info!(
            "Refreshed {} ticker(s) in {} seconds",
            self.tickers.len(),
            start_time.elapsed().as_millis() as f64 / 1000.0
        );

        Ok(())
    }

    pub async fn maybe_fix_refund_order(&mut self, refund_height: i32) -> Result<bool, Box<dyn Error>> {
        if self.network != Network::Bitcoin {
            return Ok(false);
        }
        let current_height = self.get_current_block_height().await?;
        if current_height < refund_height {
            return Ok(false);
        }

        // Check if first event of refund_height is '.com' ticker
        if self.light_client_mode {
            let row = sqlx::query!(
                "SELECT id, event_type, event->>'tick' AS tick FROM brc20_light_events WHERE block_height = $1 AND inscription_id LIKE '%00000000i0' ORDER BY id ASC LIMIT 1",
                refund_height
            ).fetch_one(&self.client).await;
            if let Ok(row) = row {
                if row.tick.as_deref() == Some(".com") {
                    return Ok(false);
                }
            } else {
                return Err("BRC20 Swap Refund events not found".into());
            }
        } else {
            let row = sqlx::query!(
                "SELECT id, event_type, event->>'tick' AS tick FROM brc20_events WHERE block_height = $1 AND inscription_id LIKE '%00000000i0' ORDER BY id ASC LIMIT 1",
                refund_height
            ).fetch_one(&self.client).await;
            if let Ok(row) = row {
                if row.tick.as_deref() == Some(".com") {
                    return Ok(false);
                }
            } else {
                return Err("BRC20 Swap Refund events not found".into());
            }
        }

        let mut tx = self.client.begin().await?;

        // Fix the order of refund events at block height 932888
        sqlx::raw_sql(format!("
            WITH filtered AS (
                SELECT
                    id,
                    row_number() OVER (ORDER BY case when event->>'tick' like '.com' then 'aaaa' else event->>'tick' end, id) AS rn_tick
                FROM {}
                WHERE block_height = 932888
                    AND inscription_id LIKE '%00000000000000000000000000i0'
                ),
            ids_sorted AS (
                SELECT
                    id,
                    row_number() OVER (ORDER BY id) AS rn_id
                FROM filtered
            ),
            mapping AS (
                SELECT
                    f.id        AS old_id,
                    i.id        AS new_id
                FROM filtered f
                JOIN ids_sorted i
                    ON i.rn_id = f.rn_tick
            )
            UPDATE {} t
            SET id = -m.new_id
            FROM mapping m
            WHERE t.id = m.old_id;
        ", self.events_table, self.events_table).as_str()).execute(&mut *tx).await?;

        sqlx::raw_sql(
            format!(
                "UPDATE {}
                SET id = -id
                WHERE id < 0;",
                self.events_table
            )
            .as_str(),
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        // Recalculate cumulative event hashes from refund_height to current_height
        self.recalculate_cumulative_event_hashes(refund_height)
            .await?;

        Ok(true)
    }

    pub async fn recalculate_cumulative_event_hashes(
        &mut self,
        from_height: i32,
    ) -> Result<(), Box<dyn Error>> {
        self.init().await?;
        let current_height = self.get_current_block_height().await?;
        for height in from_height..=current_height {
            let block_event_str = self.get_block_events_str(height).await?;
            let block_events_hash = sha256::digest(
                block_event_str
                    .as_deref()
                    .unwrap_or("")
                    .trim_end_matches(EVENT_SEPARATOR),
            );
            let cumulative_event_hash = self.get_cumulative_events_hash(height - 1).await?;
            let cumulative_event_hash = match cumulative_event_hash {
                Some(hash) => sha256::digest(hash + &block_events_hash),
                None => block_events_hash.clone(),
            };
            sqlx::query!(
                "UPDATE brc20_cumulative_event_hashes SET block_event_hash = $1, cumulative_event_hash = $2 WHERE block_height = $3",
                block_events_hash,
                cumulative_event_hash,
                height
            )
            .execute(&self.client)
            .await?;
        }
        Ok(())
    }

    pub async fn requires_trace_hash_upgrade(&self) -> Result<bool, Box<dyn Error>> {
        static TRACE_HASH_UPGRADE_EVENT_HASH_VERSION: i32 = 2;
        if sqlx::query!("SELECT event_hash_version FROM brc20_indexer_version LIMIT 1")
            .fetch_one(&self.client)
            .await?
            .event_hash_version
            == TRACE_HASH_UPGRADE_EVENT_HASH_VERSION
        {
            return Ok(true);
        } else {
            return Ok(false);
        }
    }

    pub async fn update_trace_hash(
        &self,
        block_height: i32,
        block_trace_hash: &str,
    ) -> Result<(), Box<dyn Error>> {
        let previous_cumulative_trace_hash =
            self.get_cumulative_traces_hash(block_height - 1).await?;
        let cumulative_trace_hash =
            sha256::digest(previous_cumulative_trace_hash.unwrap_or_default() + block_trace_hash);
        tracing::debug!(
            "Updating trace hash for block height {}: block_trace_hash={}, cumulative_trace_hash={}",
            block_height,
            block_trace_hash,
            cumulative_trace_hash
        );
        sqlx::query!(
            "UPDATE brc20_cumulative_event_hashes SET block_trace_hash = $1, cumulative_trace_hash = $2 WHERE block_height = $3",
            block_trace_hash,
            cumulative_trace_hash,
            block_height
        )
        .execute(&self.client)
        .await?;
        Ok(())
    }

    pub async fn update_event_hash_and_indexer_version(&self) -> Result<(), Box<dyn Error>> {
        sqlx::query!(
            "UPDATE brc20_indexer_version SET event_hash_version = $1, indexer_version = $2",
            EVENT_HASH_VERSION,
            if self.light_client_mode {
                LIGHT_CLIENT_VERSION
            } else {
                INDEXER_VERSION
            }
        )
        .execute(&self.client)
        .await?;
        Ok(())
    }

    pub async fn fetch_current_event_id(&mut self) -> Result<(), Box<dyn Error>> {
        self.current_event_id = if self.light_client_mode {
            sqlx::query!("SELECT COALESCE(MAX(id), -1) AS max_event_id FROM brc20_light_events")
                .fetch_optional(&self.client)
                .await?
                .map(|row| row.max_event_id.unwrap_or(-1))
                .unwrap_or(-1)
                + 1
        } else {
            sqlx::query!("SELECT COALESCE(MAX(id), -1) AS max_event_id FROM brc20_events")
                .fetch_optional(&self.client)
                .await?
                .map(|row| row.max_event_id.unwrap_or(-1))
                .unwrap_or(-1)
                + 1
        };
        Ok(())
    }

    pub async fn get_block_events_str(
        &self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        if self.light_client_mode {
            let row = sqlx::query!(
            "SELECT event_type, inscription_id, event FROM brc20_light_events WHERE block_height = $1 ORDER BY id ASC",
            block_height
        )
        .fetch_all(&self.client)
        .await?;
            let mut block_event_str = Vec::new();
            for row in row {
                block_event_str.push(load_event_str(
                    row.event_type,
                    &row.event,
                    &row.inscription_id,
                    &self.tickers,
                )?);
            }
            if block_event_str.is_empty() {
                return Ok(None);
            }
            Ok(Some(block_event_str.join(EVENT_SEPARATOR)))
        } else {
            let row = sqlx::query!(
            "SELECT event_type, inscription_id, event FROM brc20_events WHERE block_height = $1 ORDER BY id ASC",
            block_height
        )
        .fetch_all(&self.client)
        .await?;
            let mut block_event_str = Vec::new();
            for row in row {
                block_event_str.push(load_event_str(
                    row.event_type,
                    &row.event,
                    &row.inscription_id,
                    &self.tickers,
                )?);
            }
            if block_event_str.is_empty() {
                return Ok(None);
            }
            Ok(Some(block_event_str.join(EVENT_SEPARATOR)))
        }
    }

    pub async fn set_block_hashes(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_traces_hash: &str,
    ) -> Result<(), Box<dyn Error>> {
        let mut tx = self.client.begin().await?;

        tracing::debug!(
            "Setting block hash for height {}: {}",
            block_height,
            block_hash
        );

        // Set block hash for the given block height
        sqlx::query!(
            "INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES ($1, $2)",
            block_height,
            block_hash
        )
        .execute(&mut *tx)
        .await?;

        // Set cumulative event hashes for the block
        let block_events_hash = sha256::digest(
            self.block_event_strings
                .get(&block_height)
                .unwrap_or(&String::new())
                .trim_end_matches(EVENT_SEPARATOR),
        );
        let cumulative_event_hash = self.get_cumulative_events_hash(block_height - 1).await?;
        let cumulative_event_hash = match cumulative_event_hash {
            Some(hash) => sha256::digest(hash + &block_events_hash),
            None => block_events_hash.clone(),
        };

        let previous_cumulative_trace_hash =
            self.get_cumulative_traces_hash(block_height - 1).await?;
        let cumulative_trace_hash = if !block_traces_hash.is_empty() {
            sha256::digest(previous_cumulative_trace_hash.unwrap_or_default() + block_traces_hash)
        } else {
            String::new()
        };

        self.block_event_strings.remove(&block_height);

        sqlx::query!(
            "INSERT INTO brc20_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash, block_trace_hash, cumulative_trace_hash) VALUES ($1, $2, $3, $4, $5)",
            block_height,
            block_events_hash,
            cumulative_event_hash,
            block_traces_hash,
            cumulative_trace_hash
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub fn add_ticker(&mut self, ticker: &Ticker) -> Result<(), Box<dyn Error>> {
        if self.tickers.contains_key(&ticker.ticker) {
            return Err(format!("Ticker {} already exists", ticker.ticker).into());
        }
        self.tickers.insert(ticker.ticker.clone(), ticker.clone());

        self.new_tickers.push(ticker.clone());

        Ok(())
    }

    pub fn update_ticker(&mut self, updated_ticker: Ticker) -> Result<(), Box<dyn Error>> {
        if !self.tickers.contains_key(&updated_ticker.ticker) {
            return Err(format!("Ticker {} not found", updated_ticker.ticker).into());
        }
        self.tickers
            .insert(updated_ticker.ticker.clone(), updated_ticker.clone());

        self.ticker_updates.insert(
            updated_ticker.ticker.clone(),
            TickerUpdateData {
                remaining_supply: updated_ticker.remaining_supply,
                burned_supply: updated_ticker.burned_supply,
            },
        );

        Ok(())
    }

    pub fn get_ticker(&self, ticker: &str) -> Result<Option<Ticker>, Box<dyn Error>> {
        if let Some(ticker) = self.tickers.get(ticker) {
            return Ok(Some(ticker.clone()));
        }

        Ok(None)
    }

    async fn get_tickers(&self) -> Result<Vec<Ticker>, Box<dyn Error>> {
        let rows = sqlx::query!(
            "SELECT tick, original_tick, max_supply, remaining_supply, burned_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id, block_height FROM brc20_tickers",
        )
        .fetch_all(&self.client)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Ticker {
                ticker: row.tick,
                _max_supply: row.max_supply.to_u128().unwrap(),
                remaining_supply: row.remaining_supply.to_u128().unwrap(),
                burned_supply: row.burned_supply.to_u128().unwrap(),
                limit_per_mint: row.limit_per_mint.to_u128().unwrap(),
                decimals: row.decimals.to_u8().unwrap(),
                is_self_mint: row.is_self_mint,
                deploy_block_height: row.block_height,
                deploy_inscription_id: row.deploy_inscription_id,
                original_ticker: row.original_tick,
            })
            .collect())
    }

    pub async fn log_timer(
        &mut self,
        label: String,
        duration: u128,
        block_height: i32,
    ) -> Result<(), Box<dyn Error>> {
        if !self.save_logs {
            return Ok(());
        }
        self.log_timer_inserts
            .entry(block_height)
            .or_insert_with(HashMap::new)
            .entry(label)
            .or_insert_with(Vec::new)
            .push(duration);
        Ok(())
    }

    pub async fn get_db_version(&self) -> Result<i32, Box<dyn Error>> {
        let row = sqlx::query!("select db_version from brc20_indexer_version")
            .fetch_one(&self.client)
            .await?;
        Ok(row.db_version)
    }

    pub async fn get_next_block_height(&self) -> Result<i32, Box<dyn Error>> {
        self.get_current_block_height()
            .await
            .map(|height| height + 1)
    }

    pub async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        sqlx::query!(
            "SELECT block_height FROM brc20_block_hashes ORDER BY block_height DESC LIMIT 1"
        )
        .fetch_one(&self.client)
        .await
        .map(|row| row.block_height)
        .or_else(|_| Ok(self.first_inscription_height - 1)) // If no rows found, return first_brc20_height - 1
    }

    pub async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>> {
        let row = sqlx::query!(
            "SELECT block_hash FROM brc20_block_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_one(&self.client)
        .await?;
        Ok(row.block_hash)
    }

    pub async fn should_index_extras(
        &self,
        brc20_block_height: i32,
        ord_block_height: i32,
    ) -> Result<bool, Box<dyn Error>> {
        // does brc20_unused_txes have any rows?
        let brc20_unused_txes_is_empty = sqlx::query("SELECT 1 FROM brc20_unused_txes LIMIT 1")
            .fetch_optional(&self.client)
            .await?
            .is_none();

        if !brc20_unused_txes_is_empty {
            return Ok(true);
        }

        let brc20_current_balances_is_empty =
            sqlx::query("SELECT 1 FROM brc20_current_balances LIMIT 1")
                .fetch_optional(&self.client)
                .await?
                .is_none();

        if !brc20_current_balances_is_empty {
            return Ok(true);
        }

        if brc20_block_height > ord_block_height - 10 {
            self.initial_index_of_extra_tables().await?;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn initial_index_of_extra_tables(&self) -> Result<(), Box<dyn Error>> {
        let mut tx = self.client.begin().await?;

        tracing::info!("Resetting brc20_unused_txes");

        sqlx::query("truncate table brc20_unused_txes restart identity;")
            .execute(&mut *tx)
            .await?;

        tracing::info!("Selecting unused txes");

        let unused_txes = sqlx::query(&format!(
            "with tempp as (
                  select inscription_id, event, id, block_height
                  from {}
                  where event_type = $1
                ), tempp2 as (
                  select inscription_id, event
                  from {}
                  where event_type = $2
                )
                select t.event, t.id, t.block_height, t.inscription_id
                from tempp t
                left join tempp2 t2 on t.inscription_id = t2.inscription_id
                where t2.inscription_id is null;",
            self.events_table, self.events_table
        ))
        .bind(crate::types::events::TransferInscribeEvent::event_id())
        .bind(crate::types::events::TransferTransferEvent::event_id())
        .fetch_all(&mut *tx)
        .await?;

        tracing::info!("Inserting unused txes");

        for (index, row) in unused_txes.iter().enumerate() {
            if index % 1000 == 0 {
                tracing::info!("Inserting unused txes: {}/{}", index, unused_txes.len());
            }

            let inscription_id: String = row.get("inscription_id");
            let new_event: serde_json::Value = row.get("event");
            let event_id: i64 = row.get("id");
            let block_height: i32 = row.get("block_height");

            sqlx::query!(
                "INSERT INTO brc20_unused_txes (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)",
                inscription_id,
                new_event.get("tick").unwrap().as_str().unwrap_or(""),
                BigDecimal::from(new_event.get("amount").unwrap().as_str().unwrap().parse::<u128>().unwrap()),
                new_event.get("source_pkScript").unwrap().as_str().unwrap(),
                new_event.get("source_wallet").unwrap().as_str().unwrap(),
                event_id,
                block_height
            )
            .execute(&mut *tx)
            .await?;
        }

        tracing::info!("Resetting brc20_current_balances");

        sqlx::query("truncate table brc20_current_balances restart identity;")
            .execute(&mut *tx)
            .await?;

        tracing::info!("Selecting current balances");

        let current_balances = sqlx::query("with tempp as (
                    select max(id) as id
                    from brc20_historic_balances
                    group by pkscript, tick
                  )
                  select bhb.pkscript, bhb.tick, bhb.overall_balance, bhb.available_balance, bhb.wallet, bhb.block_height
                  from tempp t
                  left join brc20_historic_balances bhb on bhb.id = t.id
                  order by bhb.pkscript asc, bhb.tick asc;")
            .fetch_all(&mut *tx)
            .await?;

        tracing::info!("Inserting current balances");

        for (index, row) in current_balances.iter().enumerate() {
            if index % 1000 == 0 {
                tracing::info!(
                    "Inserting current balances: {}/{}",
                    index,
                    current_balances.len()
                );
            }

            let pkscript: String = row.get("pkscript");
            let tick: String = row.get("tick");
            let overall_balance: BigDecimal = row.get("overall_balance");
            let available_balance: BigDecimal = row.get("available_balance");
            let wallet: String = row.get("wallet");
            let block_height: i32 = row.get("block_height");

            sqlx::query!(
                "INSERT INTO brc20_current_balances (pkscript, tick, overall_balance, available_balance, wallet, block_height)
                    VALUES ($1, $2, $3, $4, $5, $6)",
                pkscript,
                tick,
                overall_balance,
                available_balance,
                wallet,
                block_height
            )
            .execute(&mut *tx)
            .await?;
        }

        tracing::info!("Initial index of extra tables completed");

        tx.commit().await?;

        Ok(())
    }

    pub async fn index_extra_tables(&mut self, block_height: i32) -> Result<(), Box<dyn Error>> {
        let mut tx = self.client.begin().await?;

        tracing::debug!("Indexing extra tables for block height {}", block_height);

        let balance_changes = sqlx::query(
            "select pkscript, wallet, tick, overall_balance, available_balance 
                 from brc20_historic_balances 
                 where block_height = $1
                 order by id asc;",
        )
        .bind(block_height)
        .fetch_all(&mut *tx)
        .await?;

        let mut balance_changes_map = HashMap::new();
        for row in balance_changes {
            let pkscript: String = row.get("pkscript");
            let wallet: String = row.get("wallet");
            let tick: String = row.get("tick");
            let overall_balance: BigDecimal = row.get("overall_balance");
            let available_balance: BigDecimal = row.get("available_balance");

            balance_changes_map.insert(
                (pkscript, tick),
                (wallet, overall_balance, available_balance),
            );
        }
        for ((pkscript, tick), (wallet, overall_balance, available_balance)) in balance_changes_map
        {
            sqlx::query!(
                "INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height) VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (pkscript, tick) 
                     DO UPDATE SET overall_balance = EXCLUDED.overall_balance
                                , available_balance = EXCLUDED.available_balance
                                , block_height = EXCLUDED.block_height;",
                pkscript,
                wallet,
                tick,
                overall_balance,
                available_balance,
                block_height
            )
            .execute(&mut *tx)
            .await?;
        }

        let events = sqlx::query(&format!(
            "select event, id, event_type, inscription_id 
                 from {} where block_height = $1 and (event_type = $2 or event_type = $3) 
                 order by id asc;",
            self.events_table
        ))
        .bind(block_height)
        .bind(crate::types::events::TransferInscribeEvent::event_id())
        .bind(crate::types::events::TransferTransferEvent::event_id())
        .fetch_all(&mut *tx)
        .await?;

        for row in events {
            let new_event: serde_json::Value = row.get("event");
            let event_id: i64 = row.get("id");
            let event_type: i32 = row.get("event_type");
            let inscription_id: String = row.get("inscription_id");

            if event_type == crate::types::events::TransferInscribeEvent::event_id() {
                sqlx::query!(
                    "INSERT INTO brc20_unused_txes (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                        VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (inscription_id) DO NOTHING;",
                    inscription_id,
                    new_event.get("tick").unwrap().as_str().unwrap(),
                    BigDecimal::from(new_event.get("amount").unwrap().as_str().unwrap().parse::<u128>().unwrap()),
                    new_event.get("source_pkScript").unwrap().as_str().unwrap(),
                    new_event.get("source_wallet").unwrap().as_str().unwrap(),
                    event_id,
                    block_height
                )
                .execute(&mut *tx)
                .await?;
            } else if event_type == crate::types::events::TransferTransferEvent::event_id() {
                sqlx::query!(
                    "DELETE FROM brc20_unused_txes WHERE inscription_id = $1",
                    inscription_id
                )
                .execute(&mut *tx)
                .await?;
            } else {
                panic!(
                    "Unknown event type {} for inscription_id {}",
                    event_type, inscription_id
                );
            }
        }

        tx.commit().await?;

        Ok(())
    }

    pub async fn check_residue(&self, block_height: i32) -> Result<bool, Box<dyn Error>> {
        for table_name in ALL_TABLES_WITH_BLOCK_HEIGHT.iter() {
            let row = sqlx::query(&format!(
                "SELECT COALESCE(MAX(block_height), -1) AS max_block_height FROM {}",
                table_name
            ))
            .fetch_one(&self.client)
            .await;

            if let Ok(max_block_height) =
                row.and_then(|row| Row::try_get::<i32, _>(&row, "max_block_height"))
            {
                if max_block_height != 0 && block_height < max_block_height {
                    tracing::info!(
                        "Residue found in table {}: max_block_height = {}, block_height = {}",
                        table_name,
                        max_block_height,
                        block_height
                    );
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub async fn reorg(&mut self, block_height: i32) -> Result<(), Box<dyn Error>> {
        let mut tx = self.client.begin().await?;

        tracing::info!("Starting reorg up to block height {}", block_height);

        sqlx::query("DELETE FROM brc20_tickers WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_logs WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        let res = sqlx::query(&format!(
            "SELECT event FROM {} WHERE event_type = $1 AND block_height > $2",
            self.events_table
        ))
        .bind(MintInscribeEvent::event_id())
        .bind(block_height)
        .fetch_all(&mut *tx)
        .await?;
        let mut ticker_changes = HashMap::new();
        for row in res {
            let event: MintInscribeEvent = serde_json::from_value(row.get("event"))?;
            let ticker = event.ticker.clone();
            let amount = event.amount;
            // add amount to ticker_changes
            ticker_changes
                .entry(ticker)
                .and_modify(|e| *e += amount)
                .or_insert(amount);
        }
        // Update ticker remaining_supply based on ticker_changes
        for (ticker, change) in ticker_changes {
            sqlx::query!(
                "UPDATE brc20_tickers SET remaining_supply = remaining_supply + $1 WHERE tick = $2",
                BigDecimal::from(change),
                ticker
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query("DELETE FROM brc20_historic_balances WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_bitcoin_rpc_result_cache WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_events WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_light_events WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_cumulative_event_hashes WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_bitcoin_rpc_result_cache_id_seq', max(id)) from brc20_bitcoin_rpc_result_cache;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_cumulative_event_hashes_id_seq', max(id)) from brc20_cumulative_event_hashes;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_tickers_id_seq', max(id)) from brc20_tickers;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_historic_balances_id_seq', max(id)) from brc20_historic_balances;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_events_id_seq', max(id)) from brc20_events;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_light_events_id_seq', max(id)) from brc20_light_events;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM brc20_block_hashes WHERE block_height > $1")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        sqlx::query("SELECT setval('brc20_block_hashes_id_seq', max(id)) from brc20_block_hashes;")
            .bind(block_height)
            .execute(&mut *tx)
            .await?;

        tracing::info!("Starting reorg on extra tables");

        let to_replace_balances = sqlx::query(
            "delete from brc20_current_balances where block_height > $1 RETURNING pkscript, tick;",
        )
        .bind(block_height)
        .fetch_all(&mut *tx)
        .await?;

        for (index, row) in to_replace_balances.iter().enumerate() {
            if index % 1000 == 0 {
                tracing::info!(
                    "Replacing current balances: {}/{}",
                    index,
                    to_replace_balances.len()
                );
            }

            let pkscript: String = row.get("pkscript");
            let tick: String = row.get("tick");

            sqlx::query!(
                "INSERT INTO brc20_current_balances (pkscript, tick, overall_balance, available_balance, wallet, block_height)
                    SELECT pkscript, tick, overall_balance, available_balance, wallet, block_height
                    FROM brc20_historic_balances
                    WHERE pkscript = $1 AND tick = $2
                    ORDER BY block_height DESC LIMIT 1",
                pkscript,
                tick
            )
            .execute(&mut *tx)
            .await?;
        }

        tracing::info!("Resetting brc20_unused_txes");

        sqlx::query("truncate table brc20_unused_txes restart identity;")
            .execute(&mut *tx)
            .await?;

        tracing::info!("Selecting unused txes");

        let unused_txes = sqlx::query(&format!(
            "with tempp as (
                  select inscription_id, event, id, block_height
                  from {}
                  where event_type = $1
                ), tempp2 as (
                  select inscription_id, event
                  from {}
                  where event_type = $2
                )
                select t.event, t.id, t.block_height, t.inscription_id
                from tempp t
                left join tempp2 t2 on t.inscription_id = t2.inscription_id
                where t2.inscription_id is null;",
            self.events_table, self.events_table
        ))
        .bind(crate::types::events::TransferInscribeEvent::event_id())
        .bind(crate::types::events::TransferTransferEvent::event_id())
        .fetch_all(&mut *tx)
        .await?;

        tracing::info!("Inserting unused txes");

        for (index, row) in unused_txes.iter().enumerate() {
            if index % 1000 == 0 {
                tracing::info!("Inserting unused txes: {}/{}", index, unused_txes.len());
            }

            let inscription_id: String = row.get("inscription_id");
            let new_event: serde_json::Value = row.get("event");
            let event_id: i64 = row.get("id");
            let block_height: i32 = row.get("block_height");

            sqlx::query!(
                "INSERT INTO brc20_unused_txes (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)",
                inscription_id,
                new_event.get("tick").unwrap().as_str().unwrap_or(""),
                BigDecimal::from(new_event.get("amount").unwrap().as_str().unwrap().parse::<u128>().unwrap()),
                new_event.get("source_pkScript").unwrap().as_str().unwrap(),
                new_event.get("source_wallet").unwrap().as_str().unwrap(),
                event_id,
                block_height
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        tracing::info!("Reorg completed up to block height {}", block_height);

        self.fetch_current_event_id().await?;

        self.tickers.clear();
        self.cached_events.clear();
        self.transfer_validity_cache.clear();
        self.balance_cache.clear();
        self.new_tickers.clear();
        self.ticker_updates.clear();
        self.event_inserts.clear();
        self.balance_updates.clear();

        for ticker in self.get_tickers().await? {
            self.tickers.insert(ticker.ticker.clone(), ticker);
        }

        Ok(())
    }

    pub async fn get_cumulative_events_hash(
        &self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(sqlx::query!(
            "SELECT cumulative_event_hash FROM brc20_cumulative_event_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_optional(&self.client)
        .await?.map(|r| r.cumulative_event_hash))
    }

    pub async fn get_cumulative_traces_hash(
        &self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(sqlx::query!(
            "SELECT cumulative_trace_hash FROM brc20_cumulative_event_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_optional(&self.client)
        .await?.map(|r| r.cumulative_trace_hash))
    }

    pub async fn get_block_events_hash(
        &self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(sqlx::query!(
            "SELECT block_event_hash FROM brc20_cumulative_event_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_optional(&self.client)
        .await?
        .map(|r| r.block_event_hash))
    }

    pub fn get_event_key<T>(&self, inscription_id: &str) -> String
    where
        T: Event,
    {
        format!("{}{}", T::event_id(), inscription_id)
    }

    pub async fn get_event_with_type<T>(
        &self,
        inscription_id: &str,
    ) -> Result<Option<T>, Box<dyn Error>>
    where
        T: Event + for<'de> Deserialize<'de>,
    {
        if let Some(event) = self
            .cached_events
            .get(self.get_event_key::<T>(inscription_id).as_str())
        {
            return serde_json::from_value(event.clone())
                .map(Some)
                .map_err(|e| e.into());
        }

        if self.light_client_mode {
            let row = sqlx::query!(
                "SELECT event FROM brc20_light_events WHERE inscription_id = $1 AND event_type = $2",
                inscription_id,
                T::event_id()
            )
            .fetch_optional(&self.client)
            .await?;

            if let Some(row) = row {
                let event: T = serde_json::from_value(row.event)?;
                Ok(Some(event))
            } else {
                Ok(None)
            }
        } else {
            let row = sqlx::query!(
                "SELECT event FROM brc20_events WHERE inscription_id = $1 AND event_type = $2",
                inscription_id,
                T::event_id()
            )
            .fetch_optional(&self.client)
            .await?;

            if let Some(row) = row {
                let event: T = serde_json::from_value(row.event)?;
                Ok(Some(event))
            } else {
                Ok(None)
            }
        }
    }

    /// Returns the event ID of the last event added to the database.
    pub fn add_event<T>(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        inscription_number: &i32,
        old_satpoint: &Option<String>,
        new_satpoint: &String,
        txid: &String,
        event: &T,
        decimals: Option<u8>,
    ) -> Result<i64, Box<dyn Error>>
    where
        T: Event + Serialize + std::fmt::Debug,
    {
        self.cache_event(block_height, inscription_id, event, decimals.unwrap_or(0))?;

        self.event_inserts.push(EventRecord {
            event_id: self.current_event_id,
            event_type_id: T::event_id(),
            block_height,
            inscription_id: inscription_id.to_string(),
            inscription_number: inscription_number.clone(),
            old_satpoint: old_satpoint.clone(),
            new_satpoint: new_satpoint.clone(),
            txid: txid.clone(),
            event: serde_json::to_value(event)?,
        });

        self.current_event_id += 1;
        Ok(self.current_event_id - 1)
    }

    pub fn add_light_event<T>(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        event: &mut T,
        decimals: u8,
    ) -> Result<i64, Box<dyn Error>>
    where
        T: Event + Serialize + std::fmt::Debug,
    {
        // Wallet types are not trusted, as they are not part of the event hash data.
        // Re-calculation helps us ensure that the wallet data is accurate.
        event.calculate_wallets(self.network);

        self.cache_event(block_height, inscription_id, event, decimals)?;

        self.light_event_inserts.push(LightEventRecord {
            block_height,
            event_id: self.current_event_id,
            event_type_id: T::event_id(),
            inscription_id: inscription_id.to_string(),
            event: serde_json::to_value(event)?,
        });

        self.current_event_id += 1;
        Ok(self.current_event_id - 1)
    }

    pub fn cache_event<T>(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        event: &T,
        decimals: u8,
    ) -> Result<(), Box<dyn Error>>
    where
        T: Event + Serialize + std::fmt::Debug,
    {
        tracing::debug!(
            "Storing event string for inscription_id: {}, block_height: {}, event: {:?}",
            inscription_id,
            block_height,
            event
        );

        if self
            .cached_events
            .contains_key(&self.get_event_key::<T>(inscription_id))
        {
            return Err(format!(
                "Light event for inscription_id {} and event_type {} already exists",
                inscription_id,
                T::event_name()
            )
            .into());
        }

        self.block_event_strings
            .entry(block_height)
            .or_insert_with(String::new)
            .push_str(&format!(
                "{}{}",
                event.get_event_str(inscription_id, decimals),
                EVENT_SEPARATOR
            ));

        self.cached_events.insert(
            self.get_event_key::<T>(inscription_id),
            serde_json::to_value(event)?,
        );

        Ok(())
    }

    pub async fn get_bitcoin_rpc_request(
        &self,
        request: &Bytes,
    ) -> Result<Option<Bytes>, Box<dyn Error>> {
        if !self.bitcoin_rpc_cache_enabled {
            return Ok(None);
        }

        let Ok(request_json) = serde_json::from_slice::<serde_json::Value>(request) else {
            return Err("Failed to parse Bitcoin RPC request as JSON".into());
        };

        let request_id = request_json.get("id");

        tracing::debug!(
            "Fetching Bitcoin RPC request from cache: {:?} with id {:?}",
            request_json,
            request_id
        );

        let Some(request_json_without_id) = request_json.as_object().and_then(|obj| {
            let mut obj_clone = obj.clone();
            obj_clone.remove("id");
            Some(serde_json::Value::Object(obj_clone))
        }) else {
            return Err("Bitcoin RPC request does not contain a valid JSON object".into());
        };

        let request_method = request_json_without_id
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or("Bitcoin RPC request does not contain a method")?;

        let response_without_id = self
            .bitcoin_rpc_inserts
            .get(&request_json_without_id)
            .and_then(|x| Some(x.response.clone()));

        let response_without_id = if let Some(response_without_id) = response_without_id {
            response_without_id
        } else {
            let Some(response_without_id) = sqlx::query!(
            "SELECT response FROM brc20_bitcoin_rpc_result_cache WHERE method = $1 AND request = $2 LIMIT 1",
            request_method,
            request_json_without_id
        )
        .fetch_optional(&self.client)
        .await? else {
            return Ok(None);
        };
            response_without_id.response
        };

        let response_bytes = if let Some(id) = request_id {
            let mut response_with_id = response_without_id.clone();
            if let Some(obj) = response_with_id.as_object_mut() {
                obj.insert("id".to_string(), id.clone());
            }
            serde_json::to_vec(&response_with_id)?
        } else {
            serde_json::to_vec(&response_without_id)?
        };

        return Ok(Some(Bytes::from(response_bytes)));
    }

    pub async fn cache_bitcoin_rpc_request(
        &mut self,
        request: &[u8],
        response: &Bytes,
    ) -> Result<(), Box<dyn Error>> {
        if !self.bitcoin_rpc_cache_enabled {
            return Ok(());
        }

        let Ok(request_json) =
            serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(request))
        else {
            return Err("Failed to parse Bitcoin RPC request as JSON".into());
        };

        let request_json_without_id = request_json
            .as_object()
            .and_then(|obj| {
                let mut obj_clone = obj.clone();
                obj_clone.remove("id");
                Some(serde_json::Value::Object(obj_clone))
            })
            .unwrap_or(request_json);

        let Some(request_method) = request_json_without_id
            .get("method")
            .and_then(|m| m.as_str())
        else {
            return Err("Bitcoin RPC request does not contain a method".into());
        };

        let Ok(response_json) =
            serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&response))
        else {
            return Err("Failed to parse Bitcoin RPC response as JSON".into());
        };

        let response_json_without_id = response_json
            .as_object()
            .and_then(|obj| {
                let mut obj_clone = obj.clone();
                obj_clone.remove("id");
                Some(serde_json::Value::Object(obj_clone))
            })
            .unwrap_or(response_json);

        self.bitcoin_rpc_inserts.insert(
            request_json_without_id.clone(),
            BitcoinRpcResultRecord {
                block_height: self.get_next_block_height().await?,
                method: request_method.to_string(),
                response: response_json_without_id,
            },
        );

        Ok(())
    }

    pub async fn get_transfer_validity(
        &mut self,
        inscription_id: &str,
        inscribe_event_id: i32,
        transfer_event_id: i32,
    ) -> Result<TransferValidity, Box<dyn Error>> {
        if let Some(validity) = self.transfer_validity_cache.get(inscription_id) {
            return Ok(validity.clone());
        }

        let validity = if self.light_client_mode {
            let row = sqlx::query!(
            "SELECT COALESCE(SUM(CASE WHEN event_type = $1 THEN 1 ELSE 0 END), 0) AS inscr_cnt,
                        COALESCE(SUM(CASE WHEN event_type = $2 THEN 1 ELSE 0 END), 0) AS transfer_cnt
                        FROM brc20_light_events WHERE inscription_id = $3"
        ,inscribe_event_id, transfer_event_id, inscription_id)
        .fetch_optional(&self.client)
        .await?;

            match row {
                Some(row) => {
                    if row.inscr_cnt != Some(1) {
                        TransferValidity::Invalid
                    } else if row.transfer_cnt != Some(0) {
                        TransferValidity::Used
                    } else {
                        TransferValidity::Valid
                    }
                }
                None => TransferValidity::Invalid,
            }
        } else {
            let row = sqlx::query!(
            "SELECT COALESCE(SUM(CASE WHEN event_type = $1 THEN 1 ELSE 0 END), 0) AS inscr_cnt,
                        COALESCE(SUM(CASE WHEN event_type = $2 THEN 1 ELSE 0 END), 0) AS transfer_cnt
                        FROM brc20_events WHERE inscription_id = $3"
        ,inscribe_event_id, transfer_event_id, inscription_id)
        .fetch_optional(&self.client)
        .await?;

            match row {
                Some(row) => {
                    if row.inscr_cnt != Some(1) {
                        TransferValidity::Invalid
                    } else if row.transfer_cnt != Some(0) {
                        TransferValidity::Used
                    } else {
                        TransferValidity::Valid
                    }
                }
                None => TransferValidity::Invalid,
            }
        };

        self.transfer_validity_cache
            .insert(inscription_id.to_string(), validity.clone());
        Ok(validity)
    }

    pub fn set_transfer_validity(&mut self, inscription_id: &str, validity: TransferValidity) {
        self.transfer_validity_cache
            .insert(inscription_id.to_string(), validity);
    }

    pub async fn get_balance_all_tickers(
        &mut self,
        pkscript: &str,
    ) -> Result<BTreeMap<String, Brc20Balance>, Box<dyn Error>> {
        let rows = sqlx::query!(
            "SELECT DISTINCT ON (tick)
                tick,
                overall_balance,
                available_balance
            FROM brc20_historic_balances
            WHERE pkscript = $1
            ORDER BY tick, block_height DESC, id DESC;",
            pkscript
        )
        .fetch_all(&self.client)
        .await?;

        let mut balances = BTreeMap::new();

        for row in rows {
            let Some(overall_balance) = row.overall_balance.to_u128() else {
                return Err("Invalid overall balance".into());
            };

            let Some(available_balance) = row.available_balance.to_u128() else {
                return Err("Invalid available balance".into());
            };

            if !balances.contains_key(&row.tick) {
                let balance = Brc20Balance {
                    overall_balance,
                    available_balance,
                };
                self.balance_cache
                    .insert(format!("{}:{}", row.tick, pkscript), balance.clone());
                balances.insert(row.tick.clone(), balance);
            }
        }

        Ok(balances)
    }

    pub async fn get_balance(
        &mut self,
        ticker: &str,
        pkscript: &str,
    ) -> Result<Brc20Balance, Box<dyn Error>> {
        if let Some(balance) = self.balance_cache.get(&format!("{}:{}", ticker, pkscript)) {
            return Ok(balance.clone());
        }

        let row = sqlx::query!(
            "SELECT 
                overall_balance, available_balance
                FROM brc20_historic_balances
                WHERE pkscript = $1 AND tick = $2
                ORDER BY block_height DESC, id DESC LIMIT 1;",
            pkscript,
            ticker
        )
        .fetch_optional(&self.client)
        .await?;

        if row.is_none() {
            return Ok(Brc20Balance {
                overall_balance: 0,
                available_balance: 0,
            });
        }

        let Some(overall_balance) = row.as_ref().and_then(|r| r.overall_balance.to_u128()) else {
            return Err("Invalid overall balance".into());
        };

        let Some(available_balance) = row.as_ref().and_then(|r| r.available_balance.to_u128())
        else {
            return Err("Invalid available balance".into());
        };

        let balance = Brc20Balance {
            overall_balance,
            available_balance,
        };
        self.balance_cache
            .insert(format!("{}:{}", ticker, pkscript), balance.clone());

        Ok(balance)
    }

    pub fn update_balance(
        &mut self,
        ticker: &str,
        pkscript: &str,
        wallet: &str,
        balance: &Brc20Balance,
        block_height: i32,
        event_id: i64,
    ) -> Result<(), Box<dyn Error>> {
        self.balance_cache
            .insert(format!("{}:{}", ticker, pkscript), balance.clone());

        self.balance_updates.push(BalanceUpdateData {
            ticker: ticker.to_string(),
            pkscript: pkscript.to_string(),
            wallet: wallet.to_string(),
            overall_balance: balance.overall_balance,
            available_balance: balance.available_balance,
            block_height,
            event_id,
        });

        Ok(())
    }

    pub async fn flush_queries_to_db(&mut self) -> Result<(), Box<dyn Error>> {
        let mut tx = self.client.begin().await?;

        if !self.log_timer_inserts.is_empty() {
            let mut all_log_data = Vec::new();
            let mut all_log_block_heights = Vec::new();
            for (block_height, logs) in &self.log_timer_inserts {
                for (label, durations_ns) in logs {
                    all_log_block_heights.push(*block_height);
                    all_log_data.push(json!({
                        "label": label,
                        "total_duration_ns": durations_ns.iter().sum::<u128>(),
                        "count": durations_ns.len()
                    }));
                }
            }

            sqlx::query!(
                "INSERT INTO brc20_logs (block_height, log_data) SELECT * FROM UNNEST
                ($1::int4[], $2::jsonb[])",
                &all_log_block_heights,
                &all_log_data,
            )
            .execute(&mut *tx)
            .await?;

            self.log_timer_inserts.clear();
        }

        if !self.new_tickers.is_empty() {
            let mut all_tickers = Vec::new();
            let mut all_original_tickers = Vec::new();
            let mut all_remaining_supplies = Vec::new();
            let mut all_burned_supplies = Vec::new();
            let mut all_limit_per_mints = Vec::new();
            let mut all_decimals = Vec::new();
            let mut all_is_self_mints = Vec::new();
            let mut all_deploy_inscription_ids = Vec::new();
            let mut all_deploy_block_heights = Vec::new();
            for ticker in &self.new_tickers {
                all_tickers.push(ticker.ticker.clone());
                all_original_tickers.push(ticker.original_ticker.clone());
                all_remaining_supplies.push(BigDecimal::from(ticker.remaining_supply));
                all_burned_supplies.push(BigDecimal::from(ticker.burned_supply));
                all_limit_per_mints.push(BigDecimal::from(ticker.limit_per_mint));
                all_decimals.push(ticker.decimals as i32);
                all_is_self_mints.push(ticker.is_self_mint);
                all_deploy_inscription_ids.push(ticker.deploy_inscription_id.clone());
                all_deploy_block_heights.push(ticker.deploy_block_height);
            }

            sqlx::query!(
                "INSERT INTO brc20_tickers (tick, original_tick, max_supply, remaining_supply, burned_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id, block_height) SELECT * FROM UNNEST
                ($1::text[], $2::text[], $3::numeric(40)[], $4::numeric(40)[], $5::numeric(40)[], $6::numeric(40)[], $7::int4[], $8::boolean[], $9::text[], $10::int4[])",
                &all_tickers,
                &all_original_tickers,
                &all_remaining_supplies,
                &all_remaining_supplies,
                &all_burned_supplies,
                &all_limit_per_mints,
                &all_decimals,
                &all_is_self_mints,
                &all_deploy_inscription_ids,
                &all_deploy_block_heights
            )
            .execute(&mut *tx)
            .await?;

            self.new_tickers.clear();
        }

        if !self.ticker_updates.is_empty() {
            for (ticker_name, update_data) in &self.ticker_updates {
                sqlx::query!(
                    "UPDATE brc20_tickers SET remaining_supply = $1, burned_supply = $2 WHERE tick = $3",
                    BigDecimal::from(update_data.remaining_supply),
                    BigDecimal::from(update_data.burned_supply),
                    ticker_name
                )
                .execute(&mut *tx)
                .await?;
            }
            self.ticker_updates.clear();
        }

        if !self.event_inserts.is_empty() {
            let mut all_event_ids = Vec::new();
            let mut all_event_type_ids = Vec::new();
            let mut all_block_heights = Vec::new();
            let mut all_inscription_ids = Vec::new();
            let mut all_inscription_numbers = Vec::new();
            let mut all_old_satpoints = Vec::new();
            let mut all_new_satpoints = Vec::new();
            let mut all_txids = Vec::new();
            let mut all_events = Vec::new();
            for event_data in &self.event_inserts {
                all_event_ids.push(event_data.event_id);
                all_event_type_ids.push(event_data.event_type_id);
                all_block_heights.push(event_data.block_height);
                all_inscription_ids.push(event_data.inscription_id.clone());
                all_inscription_numbers.push(event_data.inscription_number);
                all_old_satpoints
                    .push(event_data.old_satpoint.clone().unwrap_or_else(|| "".into()));
                all_new_satpoints.push(event_data.new_satpoint.clone());
                all_txids.push(event_data.txid.clone());
                all_events.push(event_data.event.clone());
            }
            sqlx::query!(
                "INSERT INTO brc20_events (id, event_type, block_height, inscription_id, inscription_number, old_satpoint, new_satpoint, txid, event) SELECT * FROM UNNEST
                ($1::bigint[], $2::int4[], $3::int4[], $4::text[], $5::int4[], $6::text[], $7::text[], $8::text[], $9::jsonb[])",
                &all_event_ids,
                &all_event_type_ids,
                &all_block_heights,
                &all_inscription_ids,
                &all_inscription_numbers,
                &all_old_satpoints,
                &all_new_satpoints,
                &all_txids,
                &all_events
            )
            .execute(&mut *tx)
            .await?;

            self.event_inserts.clear();
        }

        if !self.light_event_inserts.is_empty() {
            let mut all_event_ids = Vec::new();
            let mut all_event_type_ids = Vec::new();
            let mut all_block_heights = Vec::new();
            let mut all_inscription_ids = Vec::new();
            let mut all_events = Vec::new();
            for event_data in &self.light_event_inserts {
                all_event_ids.push(event_data.event_id);
                all_event_type_ids.push(event_data.event_type_id);
                all_block_heights.push(event_data.block_height);
                all_inscription_ids.push(event_data.inscription_id.clone());
                all_events.push(event_data.event.clone());
            }
            sqlx::query!(
                "INSERT INTO brc20_light_events (id, event_type, block_height, inscription_id, event) SELECT * FROM UNNEST
                ($1::bigint[], $2::int4[], $3::int4[], $4::text[], $5::jsonb[])",
                &all_event_ids,
                &all_event_type_ids,
                &all_block_heights,
                &all_inscription_ids,
                &all_events
            )
            .execute(&mut *tx)
            .await?;

            self.light_event_inserts.clear();
        }

        if !self.bitcoin_rpc_inserts.is_empty() {
            let mut all_methods = Vec::new();
            let mut all_requests = Vec::new();
            let mut all_responses = Vec::new();
            let mut all_block_heights = Vec::new();
            for (request, result) in &self.bitcoin_rpc_inserts {
                all_requests.push(request.clone());
                all_methods.push(result.method.clone());
                all_responses.push(result.response.clone());
                all_block_heights.push(result.block_height);
            }
            sqlx::query!(
                "INSERT INTO brc20_bitcoin_rpc_result_cache (method, request, response, block_height) SELECT * FROM UNNEST
                ($1::text[], $2::jsonb[], $3::jsonb[], $4::int4[])",
                &all_methods,
                &all_requests,
                &all_responses,
                &all_block_heights
            )
            .execute(&mut *tx)
            .await?;

            self.bitcoin_rpc_inserts.clear();
        }

        if !self.balance_updates.is_empty() {
            let mut all_pkscripts = Vec::new();
            let mut all_wallets = Vec::new();
            let mut all_tickers = Vec::new();
            let mut all_overall_balances = Vec::new();
            let mut all_available_balances = Vec::new();
            let mut all_block_heights = Vec::new();
            let mut all_event_ids = Vec::new();
            for balance_update in &self.balance_updates {
                all_pkscripts.push(balance_update.pkscript.clone());
                all_wallets.push(balance_update.wallet.clone());
                all_tickers.push(balance_update.ticker.clone());
                all_overall_balances.push(BigDecimal::from(balance_update.overall_balance));
                all_available_balances.push(BigDecimal::from(balance_update.available_balance));
                all_block_heights.push(balance_update.block_height);
                all_event_ids.push(balance_update.event_id);
            }
            sqlx::query!(
                "INSERT INTO brc20_historic_balances (pkscript, wallet, tick,
                overall_balance, available_balance, block_height, event_id) SELECT * FROM UNNEST
                ($1::text[], $2::text[], $3::text[], $4::numeric(40)[], $5::numeric(40)[], $6::int4[], $7::int8[])",
                &all_pkscripts,
                &all_wallets,
                &all_tickers,
                &all_overall_balances,
                &all_available_balances,
                &all_block_heights,
                &all_event_ids
            )
            .execute(&mut *tx)
            .await?;

            self.balance_updates.clear();
        }

        tx.commit().await?;
        Ok(())
    }

    pub fn clear_caches(&mut self) {
        if !self.new_tickers.is_empty()
            || !self.ticker_updates.is_empty()
            || !self.event_inserts.is_empty()
            || !self.balance_updates.is_empty()
        {
            panic!("clear caches called while there are pending updates");
        }
        self.transfer_validity_cache.clear();
        self.cached_events.clear();
        self.balance_cache.clear();
    }

    pub async fn reset(&mut self) -> Result<(), Box<dyn Error>> {
        sqlx::raw_sql(&SqlFiles::get_sql_file("db_reset.sql").unwrap())
            .execute(&self.client)
            .await?;

        Ok(())
    }
}
