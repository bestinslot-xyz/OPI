use std::error::Error;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::Brc20IndexerConfig;
use crate::database::Brc20Database;

// Use tokio to run the balance server
pub async fn run_balance_server(
    config: Brc20IndexerConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing::info!(
        "BRC20 Prog Balance Server running on {}",
        config.brc20_prog_balance_server_url
    );
    let listener = TcpListener::bind(config.brc20_prog_balance_server_url.clone()).await?;
    let database = Brc20Database::new(&config);

    loop {
        let (mut socket, _) = listener.accept().await?;
        let database = database.clone();
        tokio::spawn(async move {
            let mut buffer = [0; 1024];
            // Read get parameters pkscript and ticker from the socket
            match socket.read(&mut buffer).await {
                Ok(0) => return, // Connection closed
                Ok(n) => {
                    let request = String::from_utf8_lossy(&buffer[..n]);
                    let request = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("")
                        .split('?')
                        .nth(1)
                        .unwrap_or("");
                    tracing::debug!("Received request: {}", request);
                    let parts: Vec<&str> = request.split('&').collect();

                    if parts.len() != 2 {
                        socket
                            .write_all(b"Invalid request format, expected pkscript and ticker")
                            .await
                            .unwrap();
                        return;
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
                                socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nInvalid ticker format").await.unwrap();
                                return;
                            };

                            let Ok(part) = String::from_utf8(part) else {
                                socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nInvalid ticker format").await.unwrap();
                                return;
                            };

                            ticker = part;
                        }
                    }

                    // Call the balance function
                    let Ok(balance) = database.get_balance(ticker.as_str(), pkscript).await else {
                        socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\n\r\nFailed to get balance").await.unwrap();
                        return;
                    };

                    // Send the balance back to the client
                    if let Err(e) = socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n{}",
                                balance.overall_balance.to_string()
                            )
                            .as_bytes(),
                        )
                        .await
                    {
                        tracing::error!("Failed to send response: {}", e);
                    }
                }
                Err(e) => tracing::error!("Failed to read from socket: {}", e),
            }
        });
    }
}
