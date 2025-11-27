use std::error::Error;

use crate::config::{Brc20IndexerConfig, INDEXER_VERSION, LIGHT_CLIENT_VERSION};

pub struct Brc20Reporter {
    pub report_url: String,
    pub report_retries: i32,
    pub report_name: String,
    pub light_client_mode: bool,
    pub indexer_version: String,
    pub network_type: String,
    pub client: reqwest::Client,
}

impl Brc20Reporter {
    pub fn new(config: &Brc20IndexerConfig) -> Self {
        Brc20Reporter {
            report_url: config.report_url.clone(),
            report_retries: config.report_retries,
            report_name: config.report_name.clone(),
            light_client_mode: config.light_client_mode,
            indexer_version: if config.light_client_mode {
                LIGHT_CLIENT_VERSION.to_string()
            } else {
                INDEXER_VERSION.to_string()
            },
            network_type: config.network_type_string.clone(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn report(
        &self,
        block_height: i32,
        block_hash: String,
        block_time: Option<i64>,
        block_event_hash: String,
        cumulative_event_hash: String,
        block_trace_hash: String,
        cumulative_trace_hash: String,
    ) -> Result<(), Box<dyn Error>> {
        let report = serde_json::json!({
            "name": self.report_name,
            "type": "brc20",
            "node_type": if self.light_client_mode {
                "light_node"
            } else {
                "full_node"
            },
            "network_type": self.network_type,
            "version": self.indexer_version,
            "db_version": crate::config::DB_VERSION,
            "event_hash_version": crate::config::EVENT_HASH_VERSION,
            "block_height": block_height,
            "block_hash": block_hash,
            "block_time": block_time,
            "block_event_hash": block_event_hash,
            "cumulative_event_hash": cumulative_event_hash,
            "block_trace_hash": block_trace_hash,
            "cumulative_trace_hash": cumulative_trace_hash,
        });

        let mut retries = 0;
        loop {
            match self
                .client
                .post(&self.report_url)
                .json(&report)
                .send()
                .await?
                .error_for_status()
            {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if retries >= self.report_retries {
                        tracing::warn!(
                            "Failed to report BRC20 event hashes after {} retries: {}",
                            self.report_retries,
                            e
                        );
                        return Err(Box::new(e));
                    }
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                }
            }
        }
    }
}
