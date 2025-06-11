use core::panic;
use std::{collections::HashMap, error::Error, time::Instant, vec};

use brc20_index::types::events;
use lazy_static::lazy_static;
use num_traits::ToPrimitive;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use sqlx::{postgres::{PgPoolOptions}, types::BigDecimal, Pool, Postgres, Row};

use crate::{
    config::{Brc20IndexerConfig, EVENT_SEPARATOR},
    types::{Ticker, events::Event},
};

lazy_static! {
    static ref ALL_TABLES_WITH_BLOCK_HEIGHT: Vec<&'static str> = vec![
        "brc20_tickers",
        "brc20_historic_balances",
        "brc20_events",
        "brc20_cumulative_event_hashes",
        "brc20_block_hashes",
        "brc20_unused_tx_inscrs",
        "brc20_current_balances",
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
pub struct EventInsertData {
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
pub struct Brc20Database {
    pub client: Pool<Postgres>,
    pub first_inscription_height: i32,
    pub transfer_validity_cache: HashMap<String, TransferValidity>,
    pub current_event_id: i64,
    pub tickers: HashMap<String, Ticker>,
    pub cached_events: HashMap<String, serde_json::Value>,
    pub balance_cache: HashMap<String, Brc20Balance>,

    pub new_tickers: Vec<Ticker>,
    pub ticker_updates: HashMap<String, TickerUpdateData>,
    pub event_inserts: Vec<EventInsertData>,
    pub balance_updates: Vec<BalanceUpdateData>,
}

impl Brc20Database {
    pub fn new(config: &Brc20IndexerConfig) -> Self {
        let ssl_mode = if config.db_ssl {
            "?sslmode=require"
        } else {
            ""
        };
        let client = PgPoolOptions::new()
            .max_connections(5)
            .connect_lazy(&format!(
                "postgres://{}:{}@{}:{}/{}{}",
                config.db_user.replace("/", "%2F").replace(":", "%3A").replace("@", "%40"),
                config.db_password.replace("/", "%2F").replace(":", "%3A").replace("@", "%40"),
                config.db_host,
                config.db_port,
                config.db_database.replace("/", "%2F").replace(":", "%3A").replace("@", "%40"),
                ssl_mode
            ))
            .expect("Failed to connect to the database");
        Brc20Database {
            client,
            first_inscription_height: config.first_inscription_height,
            transfer_validity_cache: HashMap::new(),
            current_event_id: 0,
            tickers: HashMap::new(),
            cached_events: HashMap::new(),
            balance_cache: HashMap::new(),

            new_tickers: Vec::new(),
            ticker_updates: HashMap::new(),
            event_inserts: Vec::new(),
            balance_updates: Vec::new(),
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

        self.current_event_id =
            sqlx::query!("SELECT COALESCE(MAX(id), -1) AS max_event_id FROM brc20_events")
                .fetch_optional(&self.client)
                .await?
                .map(|row| row.max_event_id.unwrap_or(-1))
                .unwrap_or(-1)
                + 1;

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

    pub async fn set_block_hash(
        &self,
        block_height: i32,
        block_hash: &str,
    ) -> Result<(), Box<dyn Error>> {
        tracing::debug!(
            "Setting block hash for height {}: {}",
            block_height,
            block_hash
        );
        sqlx::query!(
            "INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES ($1, $2)",
            block_height,
            block_hash
        )
        .execute(&self.client)
        .await?;
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

        self.ticker_updates
            .insert(updated_ticker.ticker.clone(), TickerUpdateData {
                remaining_supply: updated_ticker.remaining_supply,
                burned_supply: updated_ticker.burned_supply,
            });

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
        sqlx::query("DELETE FROM brc20_tickers WHERE block_height > $1")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        let res = sqlx::query("SELECT event FROM brc20_events WHERE event_type = $1 AND block_height > $2")
            .bind(crate::types::events::MintInscribeEvent::event_id())
            .bind(block_height)
            .fetch_all(&self.client)
            .await?;
        let mut ticker_changes = HashMap::new();
        for row in res {
            let event: events::MintInscribeEvent = serde_json::from_value(row.get("event"))?;
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
            .execute(&self.client)
            .await?;
        }

        sqlx::query("DELETE FROM brc20_historic_balances WHERE block_height > $1")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("DELETE FROM brc20_events WHERE block_height > $1")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("DELETE FROM brc20_cumulative_event_hashes WHERE block_height > $1")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("SELECT setval('brc20_cumulative_event_hashes_id_seq', max(id)) from brc20_cumulative_event_hashes;")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("SELECT setval('brc20_tickers_id_seq', max(id)) from brc20_tickers;")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("SELECT setval('brc20_historic_balances_id_seq', max(id)) from brc20_historic_balances;")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("SELECT setval('brc20_events_id_seq', max(id)) from brc20_events;")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("DELETE FROM brc20_block_hashes WHERE block_height > $1")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        sqlx::query("SELECT setval('brc20_block_hashes_id_seq', max(id)) from brc20_block_hashes;")
            .bind(block_height)
            .execute(&self.client)
            .await?;

        // TODO: also handle brc20_unused_tx_inscrs and brc20_current_balances

        self.current_event_id =
            sqlx::query!("SELECT COALESCE(MAX(id), -1) AS max_event_id FROM brc20_events")
                .fetch_optional(&self.client)
                .await?
                .map(|row| row.max_event_id.unwrap_or(-1))
                .unwrap_or(-1)
                + 1;

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

    pub async fn get_cumulative_hash(
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

    pub async fn update_cumulative_hash(
        &self,
        block_height: i32,
        block_events: &str,
    ) -> Result<(String, String), Box<dyn Error>> {
        let block_events_hash = sha256::digest(block_events.trim_end_matches(EVENT_SEPARATOR));
        let cumulative_event_hash = self.get_cumulative_hash(block_height - 1).await?;
        let cumulative_event_hash = match cumulative_event_hash {
            Some(hash) => sha256::digest(hash + &block_events_hash),
            None => block_events_hash.clone(),
        };

        sqlx::query!(
            "INSERT INTO brc20_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES ($1, $2, $3)",
            block_height,
            block_events_hash,
            cumulative_event_hash
        )
        .execute(&self.client)
        .await?;
        Ok((block_events_hash, cumulative_event_hash))
    }

    pub fn get_event_key<T>(
        &self,
        inscription_id: &str,
    ) -> String
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
        if let Some(event) = self.cached_events.get(self.get_event_key::<T>(inscription_id).as_str()) {
            return serde_json::from_value(event.clone())
                .map(Some)
                .map_err(|e| e.into());
        }
        
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
    ) -> Result<i64, Box<dyn Error>>
    where
        T: Event + Serialize + std::fmt::Debug,
    {
        tracing::debug!(
            "Adding event for inscription_id: {}, event_type: {}, block_height: {}, event: {:?}",
            inscription_id,
            T::event_name(),
            block_height,
            event
        );
        if self.cached_events.contains_key(&self.get_event_key::<T>(inscription_id)) {
            return Err(format!(
                "Event for inscription_id {} and event_type {} already exists",
                inscription_id,
                T::event_name()
            )
            .into());
        }
        self.cached_events
            .insert(self.get_event_key::<T>(inscription_id), serde_json::to_value(event)?);

        self.event_inserts.push(EventInsertData {
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

    pub async fn get_transfer_validity(
        &mut self,
        inscription_id: &str,
        inscribe_event_id: i32,
        transfer_event_id: i32,
    ) -> Result<TransferValidity, Box<dyn Error>> {
        if let Some(validity) = self.transfer_validity_cache.get(inscription_id) {
            return Ok(validity.clone());
        }

        let row = sqlx::query!(
            "SELECT COALESCE(SUM(CASE WHEN event_type = $1 THEN 1 ELSE 0 END), 0) AS inscr_cnt,
                        COALESCE(SUM(CASE WHEN event_type = $2 THEN 1 ELSE 0 END), 0) AS transfer_cnt
                        FROM brc20_events WHERE inscription_id = $3"
        ,inscribe_event_id, transfer_event_id, inscription_id)
        .fetch_optional(&self.client)
        .await?;

        let validity = match row {
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
        };

        self.transfer_validity_cache
            .insert(inscription_id.to_string(), validity.clone());
        Ok(validity)
    }

    pub fn set_transfer_validity(&mut self, inscription_id: &str, validity: TransferValidity) {
        self.transfer_validity_cache
            .insert(inscription_id.to_string(), validity);
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


    pub async fn get_balance_nonmutable(
        &self,
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

        Ok(Brc20Balance {
            overall_balance,
            available_balance,
        })
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
            .execute(&self.client)
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
                .execute(&self.client)
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
                all_old_satpoints.push(event_data.old_satpoint.clone().unwrap_or_else(|| "".into()));
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
            .execute(&self.client)
            .await?;

            self.event_inserts.clear();
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
            .execute(&self.client)
            .await?;
        
            self.balance_updates.clear();
        }
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
