use std::error::Error;

use bitcoin::Network;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

use crate::types::Transfer;

pub struct OpiDatabase {
    pub client: Pool<Postgres>,
}

impl OpiDatabase {
    pub fn new(
        db_user: &str,
        db_password: &str,
        db_host: &str,
        db_port: &str,
        db_database: &str,
    ) -> Self {
        let client = PgPoolOptions::new()
            .max_connections(5)
            .connect_lazy(&format!(
                "postgres://{}:{}@{}:{}/{}",
                db_user, db_password, db_host, db_port, db_database
            ))
            .expect("Failed to connect to the database");
        OpiDatabase { client }
    }

    pub async fn get_network_type(&self) -> Result<Network, Box<dyn Error>> {
        let row = sqlx::query!("SELECT network_type FROM ord_network_type LIMIT 1")
            .fetch_one(&self.client)
            .await?;
        match row.network_type.as_str() {
            "mainnet" => Ok(Network::Bitcoin),
            "testnet" => Ok(Network::Testnet),
            "testnet4" => Ok(Network::Testnet4),
            "regtest" => Ok(Network::Regtest),
            "signet" => Ok(Network::Signet),
            _ => Err("ord_network_type not found, main db needs to be recreated from scratch or fixed with index.js, please run index.js in main_index".into()),
        }
    }

    pub async fn get_max_transfer_count(&self) -> Result<i32, Box<dyn Error>> {
        let row = sqlx::query!(
            "SELECT max_transfer_cnt from ord_transfer_counts WHERE event_type = 'default';"
        )
        .fetch_one(&self.client)
        .await?;
        Ok(row.max_transfer_cnt)
    }

    pub async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        let row = sqlx::query!(
            "SELECT block_height FROM block_hashes ORDER BY block_height DESC LIMIT 1"
        )
        .fetch_one(&self.client)
        .await?;
        Ok(row.block_height)
    }

    pub async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>> {
        let row = sqlx::query!(
            "SELECT block_hash FROM block_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_one(&self.client)
        .await?;
        Ok(row.block_hash)
    }

    pub async fn get_block_hash_and_time(
        &self,
        block_height: i32,
    ) -> Result<(String, i64), Box<dyn Error>> {
        let row = sqlx::query!(
            "SELECT block_hash, block_timestamp FROM block_hashes WHERE block_height = $1",
            block_height
        )
        .fetch_one(&self.client)
        .await?;
        Ok((row.block_hash, row.block_timestamp.unix_timestamp()))
    }

    pub async fn get_transfers(&self, block_height: i32) -> Result<Vec<Transfer>, Box<dyn Error>> {
        Ok(sqlx::query!(
            r#"SELECT
                ot.id,
                ot.inscription_id,
                ot.old_satpoint,
                ot.new_pkscript,
                ot.new_wallet,
                ot.sent_as_fee,
                oc."content",
                oc.byte_len,
                oc.content_type,
                onti.parent_id
                FROM ord_transfers ot
                LEFT JOIN ord_content oc ON ot.inscription_id = oc.inscription_id
                LEFT JOIN ord_number_to_id onti ON ot.inscription_id = onti.inscription_id
                WHERE ot.block_height = $1
                    AND onti.cursed_for_brc20 = false
                    AND oc."content" is not null
                    AND (oc."content"->>'p'='brc-20' 
                        OR oc."content"->>'p'='brc20-prog'
                        OR (oc."content"->>'p'='brc20-module' AND oc."content"->>'module'='BRC20PROG'))
                ORDER BY ot.id asc;"#,
            block_height
        ).fetch_all(&self.client)
        .await?.iter().map(|row|
            Transfer {
                tx_id: row.id,
                inscription_id: row.inscription_id.clone(),
                old_satpoint: none_if_empty(row.old_satpoint.clone()),
                new_pkscript: row.new_pkscript.clone(),
                new_wallet: none_if_empty(row.new_wallet.clone()),
                sent_as_fee: row.sent_as_fee,
                content: row.content.clone(),
                byte_length: row.byte_len,
                content_type: row.content_type.clone(),
                parent_inscription_id: none_if_empty(row.parent_id.clone()),
            }
        ).collect())
    }
}

fn none_if_empty(s: Option<String>) -> Option<String> {
    if let Some(s) = s {
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}
