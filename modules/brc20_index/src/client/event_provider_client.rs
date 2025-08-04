use std::error::Error;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::config::{Brc20IndexerConfig, EVENT_HASH_VERSION, OPI_URL};

pub struct EventProviderClient {
    client: reqwest::Client,
    event_providers: Vec<EventProvider>,
    network_type: String,
}

const RETRY_COUNT: i32 = 10;

impl EventProviderClient {
    pub fn new(config: &Brc20IndexerConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        // Initialize with an empty list of event providers
        Ok(EventProviderClient {
            client,
            event_providers: Vec::new(),
            network_type: config.network_type_string.clone(),
        })
    }

    pub async fn load_providers(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let response: EventProvidersResponse = self
            .client
            .get(format!(
                "{}/lc/get_verified_event_providers?event_hash_version=2",
                OPI_URL
            ))
            .send()
            .await?
            .json()
            .await?;
        self.event_providers = response.data;
        Ok(())
    }

    pub async fn get_block_info_with_retries(
        &mut self,
        block_height: i32,
    ) -> Result<BlockData, Box<dyn Error>> {
        let mut retries = 0;
        loop {
            match self.get_block_info(block_height).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    if retries >= RETRY_COUNT {
                        return Err(e.into());
                    }
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn get_block_info(
        &mut self,
        block_height: i32,
    ) -> Result<BlockData, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}/lc/get_best_hashes_for_block/{}?event_hash_version={}",
                OPI_URL, block_height, EVENT_HASH_VERSION
            ))
            .send()
            .await?;

        let response: BlockResponse = response.json().await?;

        if response.error {
            return Err("Failed to get block data".into());
        }

        response.data.ok_or("Block data not found".into())
    }

    pub async fn get_best_verified_block_with_retries(&mut self) -> Result<i32, Box<dyn Error>> {
        let mut retries = 0;
        loop {
            match self.get_best_verified_block().await {
                Ok(block) => return Ok(block),
                Err(e) => {
                    if retries >= RETRY_COUNT {
                        return Err(e);
                    }
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn get_best_verified_block(&mut self) -> Result<i32, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}/lc/get_best_verified_block?event_hash_version={}&network_type={}",
                OPI_URL, EVENT_HASH_VERSION, self.network_type
            ))
            .send()
            .await?;

        let response: BestBlockResponse = response.json().await?;

        if response.error {
            return Err("Failed to get best verified block".into());
        }

        Ok(response
            .data
            .unwrap_or_default()
            .best_verified_block
            .and_then(|block| i32::from_str_radix(&block, 10).ok())
            .unwrap_or(0))
    }

    pub async fn get_events(
        &mut self,
        block_height: i64,
    ) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
        let mut random_order = Vec::new();
        for i in 0..RETRY_COUNT {
            random_order.push(i as usize);
        }
        random_order.shuffle(&mut rand::rng());
        for i in 0..RETRY_COUNT {
            let event_provider = self
                .event_providers
                .get(random_order[i as usize] % self.event_providers.len())
                .ok_or("No event providers available")?;

            let response = self
                .client
                .get(format!(
                    "{}/v1/brc20/activity_on_block?block_height={}",
                    event_provider.url, block_height
                ))
                .send()
                .await;

            let response: BlockActivityResponse = match response {
                Ok(resp) => match resp.json().await {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::error!("Error parsing JSON response: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue; // Retry on parse error
                    }
                },
                Err(e) => {
                    tracing::error!("Error fetching events: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue; // Retry on error
                }
            };

            if let Some(error) = response.error {
                tracing::error!("Error in response: {}", error);
                continue; // Retry if there's an error
            }

            return response.result.ok_or("Event data not found".into());
        }
        return Err("Failed to fetch events after retries".into());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventProvider {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventProvidersResponse {
    error: bool,
    data: Vec<EventProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockResponse {
    pub error: bool,
    pub data: Option<BlockData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockData {
    pub best_block_hash: String,
    pub best_cumulative_hash: String,
    pub block_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockActivityResponse {
    pub error: Option<String>,
    pub result: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BestBlockResponse {
    pub error: bool,
    pub data: Option<BestBlockData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BestBlockData {
    pub best_verified_block: Option<String>,
}
