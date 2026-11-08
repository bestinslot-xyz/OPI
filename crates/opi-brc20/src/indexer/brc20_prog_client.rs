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
        sync::{Arc, Mutex, MutexGuard},
    };

    use bitcoin::Network;
    use brc20_prog::{Brc20Prog, Brc20ProgConfig};

    use crate::{
        config::{BitcoinRpcAuth, Brc20IndexerConfig},
        indexer::{BitcoinRpcBufferSlot, CachedBitcoinRpc},
    };

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
            let result = serde_json::json!({
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
            });
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

    /// A regtest indexer config whose Bitcoin RPC source reaches bitcoind at
    /// `bitcoin_rpc_url` without credentials, with the cache off.
    pub(crate) fn regtest_config(
        bitcoin_rpc_url: String,
        light_client_mode: bool,
    ) -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            bitcoin_rpc_url,
            bitcoin_rpc_auth: BitcoinRpcAuth::None,
            light_client_mode,
            bitcoin_rpc_cache_enabled: false,
            network_type: Network::Regtest,
            network_type_string: "regtest".to_string(),
            ..Default::default()
        }
    }

    /// Serialises the tests that open an engine, since opening one sets
    /// process-wide configuration and the process-wide Bitcoin RPC source.
    static ENGINE_LOCK: Mutex<()> = Mutex::new(());

    /// An engine on a fresh temp directory, removed on drop.
    pub(crate) struct TestEngine {
        pub(crate) prog: Brc20Prog,
        /// The buffer slot of the engine's Bitcoin RPC source.
        pub(crate) bitcoin_rpc_buffer: BitcoinRpcBufferSlot,
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl TestEngine {
        /// Opens a full-mode engine whose Bitcoin RPC source talks to a stub
        /// bitcoind.
        pub(crate) fn open() -> Self {
            Self::open_with(&regtest_config(spawn_bitcoin_rpc_stub(), false))
        }

        /// Opens a regtest engine on a `CachedBitcoinRpc` for `config`, as
        /// `opi-indexer` opens its engine.
        pub(crate) fn open_with(config: &Brc20IndexerConfig) -> Self {
            let guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "opi-brc20-engine-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut engine_config = Brc20ProgConfig::from_env();
            engine_config.db_path = dir.to_string_lossy().into_owned();
            engine_config.bitcoin_rpc_network = "regtest".to_string();
            let source = Arc::new(CachedBitcoinRpc::new(config).expect("the source starts"));
            let bitcoin_rpc_buffer = source.buffer_slot().clone();
            let prog = Brc20Prog::open_with_bitcoin_rpc(engine_config, source)
                .expect("engine opens on a fresh directory");
            TestEngine {
                prog,
                bitcoin_rpc_buffer,
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
        test_engine::{TestEngine, regtest_config, spawn_bitcoin_rpc_stub},
        *,
    };
    use crate::database::{Brc20Database, get_brc20_database, replace_brc20_database};

    /// The engine reaches bitcoind through the in-process source from the
    /// brc20 runtime, a current-thread runtime like this test's, while the
    /// brc20 thread can hold brc20's database lock. The source answers on its
    /// own runtime and never takes that lock, so `brc20_initialise` completes.
    #[tokio::test]
    async fn engine_initialises_through_the_in_process_source_while_the_database_is_locked() {
        let config = regtest_config(spawn_bitcoin_rpc_stub(), false);
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        replace_brc20_database(Brc20Database::new(&config)).await;
        let database = get_brc20_database();
        let held = database.lock().await;

        let engine = TestEngine::open_with(&config);
        engine
            .bitcoin_rpc_buffer
            .install(held.bitcoin_rpc_cache.buffer().clone());
        engine
            .prog
            .brc20_initialise("0".repeat(64).as_str().try_into().unwrap(), 0, 0)
            .await
            .unwrap();
    }

    /// A light client has no bitcoind; its source reports the configured
    /// network, which is all `brc20_initialise` checks.
    #[tokio::test]
    async fn a_light_engine_initialises_without_bitcoind() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let engine = TestEngine::open_with(&regtest_config(url, true));
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
