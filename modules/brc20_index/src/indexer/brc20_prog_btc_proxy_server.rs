use axum::{
    Router,
    extract::{OriginalUri, Request},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Client;
use std::net::SocketAddr;
use std::sync::LazyLock;

use crate::database::get_brc20_database;

pub async fn run_bitcoin_proxy_server(bitcoin_rpc_url: String, bitcoin_rpc_proxy_addr: String) {
    let app = Router::new().route(
        "/",
        any(move |method, uri, req| proxy(method, uri, req, bitcoin_rpc_url.clone())),
    );

    let addr: SocketAddr = bitcoin_rpc_proxy_addr.parse().expect(
        format!(
            "Invalid Bitcoin RPC proxy URL format: {}",
            bitcoin_rpc_proxy_addr
        )
        .as_str(),
    );
    tracing::info!("Bitcoin RPC proxy listening on {}", addr);
    axum_server::bind(addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn proxy(
    method: Method,
    OriginalUri(uri): OriginalUri,
    req: Request,
    bitcoin_rpc_url: String,
) -> Response {
    static CLIENT: LazyLock<Client> = LazyLock::new(|| Client::new());
    if method != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let headers = req.headers().clone();
    let body = req.into_body();

    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    match get_brc20_database()
        .lock()
        .await
        .get_bitcoin_rpc_request(&body)
        .await
    {
        Ok(Some(response)) => {
            let mut response = Response::new(response.into());
            *response.status_mut() = StatusCode::OK;
            return response;
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!("Failed to get cached Bitcoin RPC request: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Forward if not cached

    let target_uri = format!(
        "{}{}",
        bitcoin_rpc_url,
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
    );

    let forwarded = CLIENT
        .post(&target_uri)
        .body(body.clone())
        .headers(headers)
        .send()
        .await;

    match forwarded {
        Ok(resp) => {
            let status = resp.status().clone();
            let headers = resp.headers().clone();
            let response_bytes = resp.bytes().await.unwrap();

            if let Err(_) = get_brc20_database()
                .lock()
                .await
                .cache_bitcoin_rpc_request(&body, &response_bytes)
                .await
            {
                tracing::error!("Failed to cache Bitcoin RPC request");
                return StatusCode::BAD_GATEWAY.into_response();
            }

            let mut response = Response::new(response_bytes.into());
            *response.status_mut() = status;
            for (key, value) in headers {
                response.headers_mut().insert(key.unwrap(), value);
            }
            response
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}
