use std::time::Duration;

use serde_json::{Value, json};

pub const INDEXER_VERSION: &str = "opi-runes-full-node v1.0.0";
pub const DB_VERSION: i32 = 6;
/// Event hashes use the canonical event order inside each transaction.
pub const EVENT_HASH_VERSION: i32 = 2;

/// Blocks behind the journal tip within which every block is reported.
const REPORT_NEAR_TIP: u32 = 10;
/// Blocks after the last report that force a report while catching up.
const REPORT_INTERVAL: u32 = 100;

/// Reports when the block is within 10 of the journal tip or more than 100
/// past the last reported block.
pub fn should_report(height: u32, tip: u32, last_reported: u32) -> bool {
    tip.saturating_sub(height) < REPORT_NEAR_TIP
        || height.saturating_sub(last_reported) > REPORT_INTERVAL
}

pub fn payload(
    name: &str,
    network_type: &str,
    height: u32,
    block_hash: &str,
    block_event_hash: &str,
    cumulative_event_hash: &str,
) -> Value {
    json!({
        "name": name,
        "type": "runes",
        "node_type": "full_node",
        "network_type": network_type,
        "version": INDEXER_VERSION,
        "db_version": DB_VERSION,
        "event_hash_version": EVENT_HASH_VERSION,
        "block_height": height,
        "block_hash": block_hash,
        "block_event_hash": block_event_hash,
        "cumulative_event_hash": cumulative_event_hash,
    })
}

/// Posts `body` to `url`, retrying once a second up to `retries` times.
pub async fn send_with_retries(client: &reqwest::Client, url: &str, retries: u32, body: &Value) {
    for _ in 0..retries {
        match client.post(url).json(body).send().await {
            Ok(response) if response.status().as_u16() == 200 => {
                tracing::info!("reported runes hashes");
                return;
            }
            Ok(response) => {
                tracing::warn!("runes report failed with status {}", response.status());
            }
            Err(error) => tracing::warn!("runes report failed: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tracing::error!("giving up on reporting runes hashes");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_near_tip_or_after_interval() {
        assert!(should_report(995, 1000, 995));
        assert!(!should_report(900, 1000, 899));
        assert!(should_report(900, 1000, 799));
        assert!(!should_report(900, 1000, 800));
    }

    #[test]
    fn payload_has_the_report_shape() {
        let body = payload("n", "signet", 5, "h", "b", "c");
        assert_eq!(
            body,
            json!({
                "name": "n",
                "type": "runes",
                "node_type": "full_node",
                "network_type": "signet",
                "version": "opi-runes-full-node v1.0.0",
                "db_version": 6,
                "event_hash_version": 2,
                "block_height": 5,
                "block_hash": "h",
                "block_event_hash": "b",
                "cumulative_event_hash": "c",
            })
        );
    }
}
