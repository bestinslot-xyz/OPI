use crate::database::get_brc20_database;
use axum::{
    Router,
    extract::{OriginalUri, Request},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use std::net::SocketAddr;

pub async fn run_balance_server(balance_server_addr: String) {
    let app = Router::new().route("/", any(balance));

    let addr: SocketAddr = balance_server_addr.parse().expect(
        format!(
            "Invalid balance server address format: {}",
            balance_server_addr
        )
        .as_str(),
    );
    tracing::info!("Balance server listening on {}", addr);
    axum_server::bind(addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn balance(method: Method, OriginalUri(uri): OriginalUri, _: Request) -> Response {
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let Some(query) = uri.path_and_query() else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let Some(query) = query.query() else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    tracing::debug!("Received request: {}", query);
    let parts: Vec<&str> = query.split('&').collect();

    if parts.len() != 2 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut pkscript = "";
    let mut ticker: String = String::new();
    for part in parts.iter() {
        if part.starts_with("pkscript=") {
            pkscript = part
                .trim()
                .trim_start_matches("pkscript=")
                .trim_start_matches("0x");
        } else if part.starts_with("ticker=") {
            let Ok(part) = hex::decode(
                part.trim()
                    .trim_start_matches("ticker=")
                    .trim_start_matches("0x"),
            ) else {
                return StatusCode::BAD_REQUEST.into_response();
            };

            let Ok(part) = String::from_utf8(part) else {
                return StatusCode::BAD_REQUEST.into_response();
            };

            ticker = part;
        }
    }

    // Call the balance function
    match get_brc20_database()
        .lock()
        .await
        .get_balance_nonmutable(ticker.as_str(), pkscript)
        .await
    {
        Ok(balance) => {
            let mut response = Response::new(balance.overall_balance.to_string().into());
            response
                .headers_mut()
                .insert("Content-Type", "text/plain; charset=utf-8".parse().unwrap());
            *response.status_mut() = StatusCode::OK;
            return response;
        }
        Err(_) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
}
