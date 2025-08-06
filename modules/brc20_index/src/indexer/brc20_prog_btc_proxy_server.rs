use axum::{
    Router,
    extract::{OriginalUri, Request},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Client;
use std::net::SocketAddr;

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
    if method != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let target_uri = format!(
        "{}{}",
        bitcoin_rpc_url,
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
    );

    let headers = req.headers().clone();
    let body = req.into_body();

    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let client = Client::new();
    let forwarded = client
        .post(&target_uri)
        .body(body)
        .headers(headers)
        .send()
        .await;

    match forwarded {
        Ok(resp) => {
            let status = resp.status().clone();
            let headers = resp.headers().clone();
            let mut response = Response::new(resp.bytes().await.unwrap().into());
            *response.status_mut() = status;
            for (key, value) in headers {
                response.headers_mut().insert(key.unwrap(), value);
            }
            response
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}
