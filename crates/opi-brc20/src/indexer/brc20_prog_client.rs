use std::{error::Error, time::Duration};

use base64::{Engine, prelude::BASE64_STANDARD};
use brc20_prog::Brc20ProgApiClient;
use jsonrpsee::http_client::{HeaderMap, HeaderValue, HttpClient};

use crate::config::Brc20IndexerConfig;

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

pub async fn retrieve_brc20_prog_traces_hash(
    client: &HttpClient,
    block_height: i32,
) -> Result<String, Box<dyn Error>> {
    let Some(block_trace_hash) = client
        .debug_get_block_trace_hash(block_height.to_string())
        .await?
    else {
        return Err(format!(
            "Failed to get block trace hash for block height {}",
            block_height
        )
        .into());
    };
    Ok(block_trace_hash)
}
