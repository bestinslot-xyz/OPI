use std::error::Error;

use brc20_prog::{Brc20Prog, Brc20ProgApiServer};

pub async fn retrieve_brc20_prog_traces_hash(
    prog: &Brc20Prog,
    block_height: i32,
) -> Result<String, Box<dyn Error>> {
    let Some(block_trace_hash) = prog
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

#[cfg(test)]
pub(crate) mod test_engine {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        path::PathBuf,
        sync::{Mutex, MutexGuard},
    };

    use brc20_prog::{Brc20Prog, Brc20ProgConfig};

    /// The regtest result the stub gives for `method`.
    pub(crate) fn bitcoin_rpc_stub_result(method: &str) -> serde_json::Value {
        match method {
            "getnetworkinfo" => serde_json::json!({
                "version": 270000,
                "subversion": "/Satoshi:27.0.0/",
                "protocolversion": 70016,
                "localservices": "0000000000000409",
                "localrelay": true,
                "timeoffset": 0,
                "connections": 0,
                "networkactive": true,
                "networks": [],
                "relayfee": 0.00001,
                "incrementalfee": 0.00001,
                "localaddresses": [],
                "warnings": ""
            }),
            _ => serde_json::json!({
                "chain": "regtest",
                "blocks": 0,
                "headers": 0,
                "bestblockhash": "0".repeat(64),
                "difficulty": 1.0,
                "mediantime": 0,
                "verificationprogress": 1.0,
                "initialblockdownload": false,
                "chainwork": "0".repeat(64),
                "size_on_disk": 0,
                "pruned": false,
                "warnings": ""
            }),
        }
    }

    /// Answers every Bitcoin RPC call with a regtest `getblockchaininfo`,
    /// which is all `brc20_initialise` asks of bitcoind. Returns the URL.
    pub(crate) fn spawn_bitcoin_rpc_stub() -> String {
        fn answer(stream: TcpStream) -> std::io::Result<()> {
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    return Ok(());
                }
                let line = line.trim().to_ascii_lowercase();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body)?;
            let request = serde_json::from_slice::<serde_json::Value>(&body)
                .unwrap_or(serde_json::Value::Null);
            let id = request
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let result = bitcoin_rpc_stub_result(
                request
                    .get("method")
                    .and_then(|method| method.as_str())
                    .unwrap_or_default(),
            );
            let response =
                serde_json::json!({ "result": result, "error": null, "id": id }).to_string();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds a free port");
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream).ok();
            }
        });
        url
    }

    /// Serialises the tests that open an engine, since `Brc20Prog::open` sets
    /// process-wide configuration.
    static ENGINE_LOCK: Mutex<()> = Mutex::new(());

    /// An engine on a fresh temp directory, removed on drop.
    pub(crate) struct TestEngine {
        pub(crate) prog: Brc20Prog,
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl TestEngine {
        /// Opens an engine that talks to a stub bitcoind.
        pub(crate) fn open() -> Self {
            Self::open_with(spawn_bitcoin_rpc_stub)
        }

        /// Opens an engine whose Bitcoin RPC URL `bitcoin_rpc_url` returns.
        /// The engine config is set under the lock, so the closure may start
        /// services that only the engine's calls reach.
        pub(crate) fn open_with(bitcoin_rpc_url: impl FnOnce() -> String) -> Self {
            let guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "opi-brc20-engine-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut config = Brc20ProgConfig::from_env();
            config.db_path = dir.to_string_lossy().into_owned();
            config.bitcoin_rpc_url = bitcoin_rpc_url();
            config.bitcoin_rpc_network = "regtest".to_string();
            config.bitcoin_rpc_user = String::new();
            config.bitcoin_rpc_password = String::new();
            let prog = Brc20Prog::open(config).expect("engine opens on a fresh directory");
            TestEngine {
                prog,
                dir,
                _guard: guard,
            }
        }
    }

    impl Drop for TestEngine {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        test_engine::{TestEngine, bitcoin_rpc_stub_result, spawn_bitcoin_rpc_stub},
        *,
    };

    /// The engine calls the Bitcoin RPC proxy with blocking requests from the
    /// brc20 runtime, so the proxy must run on threads of its own. Hosted on
    /// this current-thread runtime, as `tokio::spawn` would, it never answers
    /// and `brc20_initialise` fails with "Bitcoin RPC unreachable".
    #[tokio::test]
    async fn engine_initialises_through_a_proxy_on_the_service_runtime() {
        use crate::{
            config::Brc20IndexerConfig,
            database::{
                BitcoinRpcResultRecord, Brc20Database, get_brc20_database, replace_brc20_database,
            },
            indexer::brc20_prog_btc_proxy_server::run_bitcoin_proxy_server,
            service_runtime::ServiceRuntime,
        };

        let proxy_addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().to_string()
        };
        let config = Brc20IndexerConfig {
            bitcoin_rpc_url: spawn_bitcoin_rpc_stub(),
            network_type_string: "regtest".to_string(),
            light_client_mode: false,
            bitcoin_rpc_cache_enabled: true,
            brc20_prog_bitcoin_rpc_proxy_server_addr: proxy_addr.clone(),
            ..Default::default()
        };

        // The results sit in the buffer, as a light client's prefetch leaves
        // them, so the proxy answers without reaching Postgres. The test holds
        // brc20's database lock throughout, as the brc20 thread can while an
        // engine call is in flight; the proxy must not need it.
        replace_brc20_database(Brc20Database::new(&config)).await;
        let database = get_brc20_database();
        let held = database.lock().await;
        let buffer = held.bitcoin_rpc_cache.buffer().clone();
        for method in ["getblockchaininfo", "getnetworkinfo"] {
            buffer.lock().unwrap().insert(
                serde_json::json!({ "method": method, "params": [], "jsonrpc": "2.0" }),
                BitcoinRpcResultRecord {
                    method: method.to_string(),
                    response: serde_json::json!({
                        "result": bitcoin_rpc_stub_result(method),
                        "error": null
                    }),
                    block_height: 1,
                },
            );
        }

        let services = ServiceRuntime::new("brc20-test-services").unwrap();
        let engine = TestEngine::open_with(|| format!("http://{proxy_addr}"));
        services
            .handle()
            .spawn(run_bitcoin_proxy_server(config, buffer));
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        engine
            .prog
            .brc20_initialise("0".repeat(64).as_str().try_into().unwrap(), 0, 0)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn in_process_engine_mines_commits_and_traces() {
        let engine = TestEngine::open();
        let prog = &engine.prog;
        prog.brc20_initialise("0".repeat(64).as_str().try_into().unwrap(), 0, 0)
            .await
            .unwrap();
        prog.brc20_mine(3, 0).await.unwrap();
        prog.brc20_commit_to_database().await.unwrap();

        let height = prog.eth_block_number().await.unwrap();
        assert_eq!(
            i32::from_str_radix(height.trim_start_matches("0x"), 16),
            Ok(3)
        );
        let trace_hash = retrieve_brc20_prog_traces_hash(prog, 3).await.unwrap();
        assert!(!trace_hash.is_empty());
        assert!(retrieve_brc20_prog_traces_hash(prog, 99).await.is_err());
    }
}
