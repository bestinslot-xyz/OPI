use std::time::Duration;

use base64::{Engine, prelude::BASE64_STANDARD};
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
        .set_headers(headers)
        .request_timeout(Duration::from_secs(5))
        .build(config.brc20_prog_rpc_url.clone())
        .expect("Failed to create HTTP client")
}
