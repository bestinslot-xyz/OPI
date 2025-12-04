use std::{error::Error, time::Duration};

use base64::{Engine, prelude::BASE64_STANDARD};
use brc20_prog::Brc20ProgApiClient;
use jsonrpsee::http_client::{HeaderMap, HeaderValue, HttpClient};

use crate::config::{Brc20IndexerConfig, EVENT_SEPARATOR};

pub fn build_brc20_prog_http_client(config: &Brc20IndexerConfig) -> HttpClient {
    let mut headers = HeaderMap::new();
    if let Some(user) = &config.brc20_prog_rpc_user {
        if let Some(password) = &config.brc20_prog_rpc_password {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!(
                    "Basic {}",
                    BASE64_STANDARD.encode(format!("{}:{}", user, password))
                ))
                .unwrap(),
            );
        }
    }
    HttpClient::builder()
        .max_request_size(u32::MAX) // This is to support large payloads as this should never fail
        .max_response_size(u32::MAX) // This is to support large payloads as this should never fail
        .set_headers(headers)
        .request_timeout(Duration::from_secs(10))
        .build(config.brc20_prog_rpc_url.clone())
        .expect("Failed to create HTTP client")
}

pub async fn calculate_brc20_prog_traces_hash(
    client: &HttpClient,
    block_height: i32,
) -> Result<String, Box<dyn Error>> {
    let traces_hash_str = calculate_brc20_prog_traces_str(client, block_height).await?;
    Ok(sha256::digest(traces_hash_str))
}

pub async fn calculate_brc20_prog_traces_str(
    client: &HttpClient,
    block_height: i32,
) -> Result<String, Box<dyn Error>> {
    let mut traces_hash_str = String::new();
    let block = client
        .eth_get_block_by_number(format!("{}", block_height), Some(true))
        .await?;
    if block.transactions.is_left() {
        if block.transactions.left().unwrap_or_default().is_empty() {
            tracing::debug!("No traces in block {}", block_height);
        } else {
            return Err(format!("Unexpected transaction format in block {}", block_height).into());
        }
    } else if let Some(mut txes) = block.transactions.right() {
        txes.sort_by_key(|tx| tx.transaction_index);
        for tx in txes {
            let Some(trace) = client.debug_trace_transaction(tx.hash).await? else {
                tracing::warn!(
                    "No trace found for transaction {:?} in block {}",
                    tx,
                    block_height
                );
                continue;
            };
            // Convert trace to JSON and hash it
            let trace_json = serde_json::to_value(&trace)?;
            let trace_hash_str = serde_json_canonicalizer::to_string(&trace_json)?;
            traces_hash_str.push_str(&trace_hash_str);
            traces_hash_str.push_str(EVENT_SEPARATOR);
        }
    }
    Ok(traces_hash_str
        .trim_end_matches(EVENT_SEPARATOR)
        .to_string())
}
