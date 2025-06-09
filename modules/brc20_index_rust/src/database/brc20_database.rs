use std::{collections::HashMap, error::Error, time::Instant};

use lazy_static::lazy_static;
use num_traits::ToPrimitive;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row, postgres::PgPoolOptions, types::BigDecimal};

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
pub struct Brc20Database {
    pub client: Pool<Postgres>,
    pub first_inscription_height: i32,
    pub transfer_validity_cache: HashMap<String, TransferValidity>,
    pub current_event_id: i64,
    pub tickers: HashMap<String, Ticker>,
    pub cached_events: HashMap<String, serde_json::Value>,
    pub balance_cache: HashMap<String, Brc20Balance>,
}

impl Brc20Database {
    pub fn new(config: &Brc20IndexerConfig) -> Self {
        let client = PgPoolOptions::new()
            .max_connections(5)
            .connect_lazy(&format!(
                "postgres://{}:{}@{}:{}/{}",
                config.db_user,
                config.db_password,
                config.db_host,
                config.db_port,
                config.db_database,
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

    pub async fn add_ticker(&mut self, ticker: &Ticker) -> Result<(), Box<dyn Error>> {
        if self.tickers.contains_key(&ticker.ticker) {
            return Err(format!("Ticker {} already exists", ticker.ticker).into());
        }
        self.tickers.insert(ticker.ticker.clone(), ticker.clone());
        sqlx::query!(
            "INSERT INTO brc20_tickers (tick, original_tick, max_supply, decimals, limit_per_mint, remaining_supply, burned_supply, block_height, is_self_mint, deploy_inscription_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            ticker.ticker,
            ticker.original_ticker,
            BigDecimal::from(ticker.remaining_supply),
            ticker.decimals as i32,
            BigDecimal::from(ticker.limit_per_mint),
            BigDecimal::from(ticker.remaining_supply),
            BigDecimal::from(0u128), // Assuming burned_supply starts at 0
            ticker.deploy_block_height,
            ticker.is_self_mint,
            ticker.deploy_inscription_id
        )
        .execute(&self.client)
        .await?;

        Ok(())
    }

    pub async fn update_ticker(&mut self, updated_ticker: Ticker) -> Result<(), Box<dyn Error>> {
        if !self.tickers.contains_key(&updated_ticker.ticker) {
            return Err(format!("Ticker {} not found", updated_ticker.ticker).into());
        }
        self.tickers
            .insert(updated_ticker.ticker.clone(), updated_ticker.clone());
        sqlx::query!(
            "UPDATE brc20_tickers SET remaining_supply = $1, burned_supply = $2 WHERE tick = $3",
            BigDecimal::from(updated_ticker.remaining_supply),
            BigDecimal::from(updated_ticker.burned_supply),
            updated_ticker.ticker
        )
        .execute(&self.client)
        .await?;
        Ok(())
    }

    pub async fn get_ticker(&self, ticker: &str) -> Result<Option<Ticker>, Box<dyn Error>> {
        if let Some(ticker) = self.tickers.get(ticker) {
            return Ok(Some(ticker.clone()));
        }
        let row = sqlx::query!(
            "SELECT tick, original_tick, max_supply, remaining_supply, burned_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id, block_height FROM brc20_tickers WHERE tick = $1",
            ticker
        )
        .fetch_optional(&self.client)
        .await?;

        if let Some(row) = row {
            Ok(Some(Ticker {
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
            }))
        } else {
            Ok(None)
        }
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
        for table_name in ALL_TABLES_WITH_BLOCK_HEIGHT.iter() {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE block_height >= $1",
                table_name
            ))
            .bind(block_height)
            .execute(&self.client)
            .await?;
        }

        self.current_event_id =
            sqlx::query!("SELECT COALESCE(MAX(id), -1) AS max_event_id FROM brc20_events")
                .fetch_optional(&self.client)
                .await?
                .map(|row| row.max_event_id.unwrap_or(-1))
                .unwrap_or(-1)
                + 1;

        self.tickers.clear();
        for ticker in self.get_tickers().await? {
            self.tickers.insert(ticker.ticker.clone(), ticker);
        }

        self.cached_events.clear();

        self.transfer_validity_cache.clear();

        self.balance_cache.clear();

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
    pub async fn add_event<T>(
        &mut self,
        block_height: i32,
        inscription_id: &str,
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
        sqlx::query!(
            "INSERT INTO brc20_events (id, event_type, block_height, inscription_id, event) VALUES ($1, $2, $3, $4, $5)",
            self.current_event_id,
            T::event_id(),
            block_height,
            inscription_id,
            serde_json::to_value(event)?
        )
        .execute(&self.client)
        .await?;
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

    pub async fn update_balance(
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

        sqlx::query!(
            "INSERT INTO brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            pkscript,
            wallet,
            ticker,
            BigDecimal::from(balance.overall_balance),
            BigDecimal::from(balance.available_balance),
            block_height,
            event_id
        )
        .execute(&self.client)
        .await?;
        Ok(())
    }

    pub async fn reset(&mut self) -> Result<(), Box<dyn Error>> {
        sqlx::raw_sql(&SqlFiles::get_sql_file("db_reset.sql").unwrap())
            .execute(&self.client)
            .await?;

        Ok(())
    }
}
