use std::error::Error;

use crate::config::Brc20IndexerConfig;

pub struct Brc20Reporter {
    pub report_to_indexer: bool,
    pub report_url: String,
    pub report_retries: i32,
    pub report_name: String,
    pub network_type: String,
    pub client: reqwest::Client,
}

impl Brc20Reporter {
    pub fn new(config: &Brc20IndexerConfig) -> Self {
        Brc20Reporter {
            report_to_indexer: config.report_to_indexer,
            report_url: config.report_url.clone(),
            report_retries: config.report_retries,
            report_name: config.report_name.clone(),
            network_type: config.network_type_string.clone(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn report(
        &self,
        block_height: i32,
        block_hash: String,
        block_event_hash: String,
        cumulative_event_hash: String,
    ) -> Result<(), Box<dyn Error>> {
        if !self.report_to_indexer {
            return Ok(());
        }
        let mut retries = 0;
        loop {
            match self
                .send_report(
                    block_height,
                    block_hash.clone(),
                    block_event_hash.clone(),
                    cumulative_event_hash.clone(),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if retries >= self.report_retries {
                        return Err(Box::new(e));
                    }
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn send_report(
        &self,
        block_height: i32,
        block_hash: String,
        block_event_hash: String,
        cumulative_event_hash: String,
    ) -> Result<(), reqwest::Error> {
        let report = serde_json::json!({
            "name": self.report_name,
            "type": "brc20",
            "node_type": "full_node",
            "network_type": self.network_type,
            "version": crate::config::INDEXER_VERSION,
            "db_version": crate::config::DB_VERSION,
            "event_hash_version": crate::config::EVENT_HASH_VERSION,
            "block_height": block_height,
            "block_hash": block_hash,
            "block_event_hash": block_event_hash,
            "cumulative_event_hash": cumulative_event_hash,
        });

        self.client
            .post(&self.report_url)
            .json(&report)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }
}
