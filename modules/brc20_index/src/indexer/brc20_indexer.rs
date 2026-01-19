use std::{cmp::min, error::Error};

use brc20_prog::Brc20ProgApiClient;
use jsonrpsee::http_client::HttpClient;
use tokio::task::JoinHandle;
use versions::{Requirement, Versioning};

use crate::{
    client::{EventProviderClient, OpiClient},
    config::{
        AMOUNT_KEY, BASE64_DATA_KEY, BRC20_MODULE_BRC20PROG, BRC20_PROG_MINE_BATCH_SIZE,
        BRC20_PROG_VERSION_REQUIREMENT, Brc20IndexerConfig, CONTRACT_ADDRESS_KEY, DATA_KEY,
        DB_VERSION, DECIMALS_KEY, HASH_KEY, INSCRIPTION_ID_KEY, LIMIT_PER_MINT_KEY, MAX_AMOUNT,
        MAX_SUPPLY_KEY, MODULE_KEY, OPERATION_BRC20_PROG_CALL, OPERATION_BRC20_PROG_CALL_SHORT,
        OPERATION_BRC20_PROG_DEPLOY, OPERATION_BRC20_PROG_DEPLOY_SHORT,
        OPERATION_BRC20_PROG_TRANSACT, OPERATION_BRC20_PROG_TRANSACT_SHORT, OPERATION_DEPLOY,
        OPERATION_KEY, OPERATION_MINT, OPERATION_PREDEPLOY, OPERATION_TRANSFER, OPERATION_WITHDRAW,
        PREDEPLOY_BLOCK_HEIGHT_ACCEPTANCE_DELAY, PREDEPLOY_BLOCK_HEIGHT_DELAY, PROTOCOL_BRC20,
        PROTOCOL_BRC20_MODULE, PROTOCOL_BRC20_PROG, PROTOCOL_KEY, SALT_KEY, SELF_MINT_KEY,
        TICKER_KEY, get_startup_wait_secs,
    },
    database::{
        get_brc20_database,
        timer::{start_timer, stop_timer},
    },
    indexer::{
        EventGenerator, EventProcessor,
        brc20_prog_btc_proxy_server::run_bitcoin_proxy_server,
        brc20_prog_client::{build_brc20_prog_http_client, retrieve_brc20_prog_traces_hash},
        brc20_reporter::Brc20Reporter,
        brc20_swap_refund::Brc20SwapRefund,
        utils::{ALLOW_ZERO, DISALLOW_ZERO, get_amount_value, get_decimals_value},
    },
    no_default,
    types::events::{
        Brc20ProgCallInscribeEvent, Brc20ProgCallTransferEvent, Brc20ProgDeployInscribeEvent,
        Brc20ProgDeployTransferEvent, Brc20ProgTransactInscribeEvent,
        Brc20ProgTransactTransferEvent, Brc20ProgWithdrawInscribeEvent,
        Brc20ProgWithdrawTransferEvent, DeployInscribeEvent, Event, MintInscribeEvent,
        PreDeployInscribeEvent, TransferInscribeEvent, TransferTransferEvent, event_name_to_id,
        load_event,
    },
};

static SPAN: &str = "Brc20Indexer";

pub struct Brc20Indexer {
    main_db: OpiClient,
    event_provider_client: EventProviderClient,
    last_opi_block: i32,
    last_reported_block: Option<i32>,
    config: Brc20IndexerConfig,
    brc20_prog_client: HttpClient,
    brc20_reporter: Brc20Reporter,
    bitcoin_proxy_server_handle: Option<JoinHandle<()>>,
}

impl Brc20Indexer {
    pub fn new(config: Brc20IndexerConfig) -> Self {
        let main_db = OpiClient::new(config.opi_db_url.clone());

        let brc20_prog_client = build_brc20_prog_http_client(&config);
        let brc20_reporter = Brc20Reporter::new(&config);
        let event_provider_client =
            EventProviderClient::new(&config).expect("Failed to create EventProviderClient");

        Brc20Indexer {
            main_db,
            config,
            last_opi_block: 0,
            last_reported_block: None,
            brc20_prog_client,
            brc20_reporter,
            event_provider_client,
            bitcoin_proxy_server_handle: None,
        }
    }

    async fn init(&mut self) -> Result<(), Box<dyn Error>> {
        get_brc20_database().lock().await.init().await?;

        tracing::info!(
            "OPI Block Height: {}",
            get_brc20_database()
                .lock()
                .await
                .get_current_block_height()
                .await?
        );

        tracing::info!(
            "Prog Block Height: {}",
            parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?
        );

        self.reorg_to_last_synced_block_height().await?; // Ensure no residue before proceeding

        if get_brc20_database()
            .lock()
            .await
            .requires_trace_hash_upgrade()
            .await?
        {
            self.regenerate_and_validate_trace_hashes().await?;
        }

        get_brc20_database()
            .lock()
            .await
            .maybe_fix_refund_order(self.config.brc20_swap_refund_activation_height)
            .await?;

        get_brc20_database()
            .lock()
            .await
            .update_event_hash_and_indexer_version()
            .await?;

        self.event_provider_client.load_providers().await?;
        self.last_opi_block = self.get_opi_block_height().await?;

        if self.config.report_to_indexer {
            self.last_reported_block = self
                .event_provider_client
                .get_best_verified_block_with_retries()
                .await
                .ok();
        }

        let db_version = get_brc20_database().lock().await.get_db_version().await?;
        if db_version != DB_VERSION {
            return Err(format!(
                "db_version mismatch, expected {}, got {}, please run brc20_indexer with --reset",
                DB_VERSION, db_version
            )
            .into());
        }

        self.clear_caches().await?;

        if self.config.brc20_prog_enabled {
            let brc20_prog_version = self.brc20_prog_client.brc20_version().await?;
            let requirement = Requirement::new(BRC20_PROG_VERSION_REQUIREMENT).expect(
                format!(
                    "Invalid BRC20_PROG_VERSION requirement: {}",
                    BRC20_PROG_VERSION_REQUIREMENT
                )
                .as_str(),
            );
            let version = Versioning::new(&brc20_prog_version)
                .expect(format!("Invalid brc20_prog version: {}", brc20_prog_version).as_str());
            if !requirement.matches(&version) {
                return Err(format!(
                    "brc20_prog version mismatch, expected {}, got {}",
                    BRC20_PROG_VERSION_REQUIREMENT, brc20_prog_version
                )
                .into());
            }

            // Wait for the servers to start
            if self.config.brc20_prog_bitcoin_rpc_proxy_server_enabled {
                let bitcoin_rpc_proxy_addr_clone =
                    self.config.brc20_prog_bitcoin_rpc_proxy_server_addr.clone();
                let bitcoin_rpc_url_clone = self.config.bitcoin_rpc_url.clone();
                let light_client_mode = self.config.light_client_mode;
                let network_type_clone = self.config.network_type_string.clone();
                self.bitcoin_proxy_server_handle = Some(tokio::spawn(async move {
                    run_bitcoin_proxy_server(
                        bitcoin_rpc_url_clone,
                        light_client_mode,
                        network_type_clone,
                        bitcoin_rpc_proxy_addr_clone,
                    )
                    .await
                }));

                let wait_seconds = get_startup_wait_secs();
                tracing::info!(
                    "Waiting for the server to start for {} seconds...",
                    wait_seconds
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds)).await;
                tracing::debug!("Continuing initialization...");
            }

            let brc20_prog_block_height =
                parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?;

            self.brc20_prog_client
                .brc20_initialise("0".repeat(64).as_str().try_into()?, 0, 0)
                .await?;

            if brc20_prog_block_height < self.config.first_brc20_prog_phase_one_height - 1 {
                let mut current_prog_height = brc20_prog_block_height;
                while current_prog_height < self.config.first_brc20_prog_phase_one_height - 1 {
                    let next_prog_height = (current_prog_height + BRC20_PROG_MINE_BATCH_SIZE)
                        .min(self.config.first_brc20_prog_phase_one_height - 1);
                    tracing::info!(
                        "BRC20 Prog initialising from block height {} to {}",
                        current_prog_height,
                        next_prog_height
                    );
                    self.brc20_prog_client
                        .brc20_mine((next_prog_height - current_prog_height) as u64, 0)
                        .await?;
                    self.brc20_prog_client.brc20_commit_to_database().await?;
                    current_prog_height = next_prog_height;
                }
            }
        }

        Ok(())
    }

    pub async fn regenerate_and_validate_trace_hashes(&mut self) -> Result<(), Box<dyn Error>> {
        tracing::info!("Updating BRC2.0 event/trace hashes...");
        let last_reported_block = self
            .event_provider_client
            .get_best_verified_block_with_retries()
            .await?;
        let start_block = min(
            if self.config.report_all_blocks {
                self.config.first_inscription_height // Re-generate all blocks
            } else {
                self.config.first_brc20_prog_phase_one_height // Only re-generate from BRC2.0 phase one
            },
            last_reported_block + 1,
        );
        let end_block = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;

        tracing::info!(
            "Starting trace hash regeneration from block {} to {}",
            start_block,
            end_block
        );

        for block_height in start_block..=end_block {
            if block_height >= self.config.first_brc20_prog_phase_one_height {
                if block_height % 1000 == 0 {
                    tracing::info!(
                        "Regenerating trace hash for block height {}/{}",
                        block_height,
                        end_block
                    );
                }
                let Some(block_trace_hash) = self
                    .brc20_prog_client
                    .debug_get_block_trace_hash(block_height.to_string())
                    .await?
                else {
                    return Err("Trace hash regeneration failed".into());
                };
                get_brc20_database()
                    .lock()
                    .await
                    .update_trace_hash(block_height, &block_trace_hash)
                    .await?;
                let Some(cumulative_trace_hash) = get_brc20_database()
                    .lock()
                    .await
                    .get_cumulative_traces_hash(block_height)
                    .await?
                else {
                    return Err("Cumulative trace hash not found".into());
                };
                if self.config.light_client_mode {
                    // Validate with OPI client if in light client mode
                    let Some(opi_trace_hash) = self
                        .event_provider_client
                        .get_block_info_with_retries(block_height)
                        .await?
                        .best_cumulative_trace_hash
                    else {
                        return Err("Failed to get OPI trace hash for validation".into());
                    };
                    if cumulative_trace_hash != opi_trace_hash {
                        return Err(format!(
                            "Trace hash mismatch at block {}: expected {}, got {}",
                            block_height, opi_trace_hash, cumulative_trace_hash
                        )
                        .into());
                    }
                }
            }
            if !self.config.light_client_mode
                && (self.config.report_all_blocks || block_height > last_reported_block)
            {
                if block_height % 1000 == 0 {
                    tracing::info!("Reporting block height {}/{}", block_height, end_block);
                }
                self.report_block(block_height).await?;
            }
        }
        tracing::info!("BRC2.0 trace hash update complete.");
        Ok(())
    }

    pub async fn clear_caches(&mut self) -> Result<(), Box<dyn Error>> {
        let current_block_height = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;

        let function_timer = start_timer(SPAN, "clear_caches", current_block_height);

        if self.config.brc20_prog_enabled {
            self.brc20_prog_client.brc20_clear_caches().await?;
        }

        if get_brc20_database()
            .lock()
            .await
            .check_residue(current_block_height)
            .await?
        {
            tracing::debug!(
                "BRC20 indexer residue found at block height {}, reorging to last synced block height",
                current_block_height
            );
            self.reorg(current_block_height).await?;
        }

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn reorg(&mut self, block_height: i32) -> Result<(), Box<dyn Error>> {
        tracing::info!("Reorganizing BRC20 indexer database...");
        get_brc20_database()
            .lock()
            .await
            .reorg(block_height)
            .await?;
        if self.config.brc20_prog_enabled
            && block_height >= self.config.first_brc20_prog_phase_one_height
        {
            self.brc20_prog_client
                .brc20_reorg(block_height as u64)
                .await?;
        }
        tracing::info!("BRC20 indexer database reorg complete.");
        Ok(())
    }

    pub async fn reset(&mut self) -> Result<(), Box<dyn Error>> {
        tracing::info!("Resetting BRC20 indexer database...");
        get_brc20_database().lock().await.reset().await?;
        get_brc20_database().lock().await.init().await?;
        tracing::info!("BRC20 indexer database reset complete.");
        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        self.init().await?;
        loop {
            let next_block = get_brc20_database()
                .lock()
                .await
                .get_next_block_height()
                .await?;

            if next_block >= self.config.height_limit {
                // Check height limit
                tracing::info!(
                    "Reached height limit of {}, stopping indexer.",
                    self.config.height_limit
                );
                return Ok(());
            }

            let loop_timer = start_timer(SPAN, "run_single_block", next_block);

            // This doesn't always reorg, but it will reorg if the last block is not the same
            let reorg_to_last_synced_block_height_timer =
                start_timer(SPAN, "reorg_to_last_synced_block_height", next_block);
            self.reorg_to_last_synced_block_height().await?;
            stop_timer(&reorg_to_last_synced_block_height_timer).await;

            let next_block = get_brc20_database()
                .lock()
                .await
                .get_next_block_height()
                .await?;

            if next_block >= self.config.height_limit {
                tracing::info!(
                    "Reached height limit of {}, stopping indexer.",
                    self.config.height_limit
                );
                return Ok(());
            }

            // Check if a new block is available
            let last_opi_block = self.last_opi_block;
            if next_block > last_opi_block {
                tracing::info!("Waiting for new blocks...");
                self.last_opi_block = self.get_opi_block_height().await.unwrap_or(last_opi_block);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }

            let is_synced = next_block == last_opi_block;

            if next_block % 1000 == 0 && next_block >= self.config.first_brc20_height {
                tracing::info!("Clearing brc20 db caches at block height {}", next_block);
                let clear_caches_timer = start_timer(SPAN, "clear_brc20_caches", next_block);
                get_brc20_database().lock().await.clear_caches();
                stop_timer(&clear_caches_timer).await;
            }
            if is_synced || next_block % 1000 == 0 {
                tracing::info!("Processing block: {}", next_block);
            }

            let get_block_info_timer = start_timer(SPAN, "get_block_info", next_block);
            let (block_hash, block_time, opi_cumulative_event_hash, opi_cumulative_trace_hash) =
                if self.config.light_client_mode {
                    let block_info = self
                        .event_provider_client
                        .get_block_info_with_retries(next_block)
                        .await?;
                    (
                        block_info.best_block_hash,
                        block_info.best_block_time.unwrap_or(0) as i64, // Default to 0 if not available
                        block_info.best_cumulative_hash,
                        block_info.best_cumulative_trace_hash,
                    )
                } else {
                    let (block_hash, block_time) =
                        self.main_db.get_block_hash_and_time(next_block).await?;
                    (block_hash, block_time, "".to_string(), None)
                };
            stop_timer(&get_block_info_timer).await;

            if block_time == 0
                && self.config.brc20_prog_enabled
                && next_block >= self.config.first_brc20_prog_phase_one_height
            {
                tracing::error!(
                    "Block time is 0 for block {}, this may cause issues with BRC2.0 events. Stopping indexing.",
                    next_block
                );
                return Err("Block time is 0".into());
            }

            if next_block < self.config.first_brc20_height {
                if next_block % 1000 == 0 {
                    tracing::info!(
                        "Block height {} is less than first_brc20_height {}, skipping",
                        next_block,
                        self.config.first_brc20_height
                    );
                    let timer = start_timer(SPAN, "flush_queries_to_db", next_block);
                    get_brc20_database()
                        .lock()
                        .await
                        .flush_queries_to_db()
                        .await?;
                    stop_timer(&timer).await;
                }
            } else {
                if self.config.light_client_mode {
                    if next_block > self.config.first_brc20_prog_phase_one_height {
                        match self.pre_fill_rpc_results_cache(next_block).await {
                            Ok(_) => {}
                            Err(err) => {
                                tracing::error!(
                                    "Failed to get Bitcoin RPC results for block {}: {}",
                                    next_block,
                                    err
                                );
                                tracing::error!("Retrying in 5 seconds...");
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        };
                    }
                    let get_events_timer = start_timer(SPAN, "get_events", next_block);
                    let events = match self
                        .event_provider_client
                        .get_events(next_block as i64)
                        .await
                    {
                        Ok(events) => events,
                        Err(err) => {
                            tracing::error!(
                                "Failed to get events for block {}: {}",
                                next_block,
                                err
                            );
                            tracing::error!("Retrying in 5 seconds...");
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                            continue;
                        }
                    };
                    stop_timer(&get_events_timer).await;
                    self.process_events(
                        next_block,
                        &block_hash,
                        block_time as u64,
                        is_synced,
                        events,
                    )
                    .await?;
                } else {
                    self.generate_and_process_events(
                        next_block,
                        &block_hash,
                        block_time as u64,
                        is_synced,
                    )
                    .await?;
                }
            }

            let index_extras_timer = start_timer(SPAN, "index_extra_tables", next_block);
            if next_block >= self.config.first_brc20_height
                && get_brc20_database()
                    .lock()
                    .await
                    .should_index_extras(next_block, last_opi_block)
                    .await?
            {
                // Index extras if synced or close to sync
                get_brc20_database()
                    .lock()
                    .await
                    .index_extra_tables(next_block)
                    .await?;
            }
            stop_timer(&index_extras_timer).await;

            let trace_hash_timer = start_timer(SPAN, "retrieve_brc20_prog_traces_hash", next_block);
            let block_traces_hash = if self.config.brc20_prog_enabled
                && next_block >= self.config.first_brc20_prog_phase_one_height
            {
                retrieve_brc20_prog_traces_hash(&self.brc20_prog_client, next_block).await?
            } else {
                String::new()
            };
            stop_timer(&trace_hash_timer).await;

            let set_block_hashes_timer = start_timer(SPAN, "set_block_hashes", next_block);
            get_brc20_database()
                .lock()
                .await
                .set_block_hashes(next_block, &block_hash, block_traces_hash.as_str())
                .await?;
            stop_timer(&set_block_hashes_timer).await;

            let Some(block_events_hash) = get_brc20_database()
                .lock()
                .await
                .get_block_events_hash(next_block)
                .await?
            else {
                tracing::warn!("Block events hash not found for block {}", next_block);
                return Ok(());
            };

            let Some(cumulative_events_hash) = get_brc20_database()
                .lock()
                .await
                .get_cumulative_events_hash(next_block)
                .await?
            else {
                tracing::warn!("Cumulative events hash not found for block {}", next_block);
                return Ok(());
            };

            let cumulative_traces_hash = get_brc20_database()
                .lock()
                .await
                .get_cumulative_traces_hash(next_block)
                .await?
                .unwrap_or_default();

            if self.config.light_client_mode {
                // Validate cumulative hash with OPI client
                if cumulative_events_hash != opi_cumulative_event_hash
                    || (self.config.brc20_prog_enabled
                        && cumulative_traces_hash
                            != opi_cumulative_trace_hash.clone().unwrap_or_default())
                {
                    // Reorg the last block if cumulative events hash mismatch
                    get_brc20_database()
                        .lock()
                        .await
                        .reorg(next_block - 1)
                        .await?;
                    if self.config.brc20_prog_enabled {
                        self.brc20_prog_client
                            .brc20_reorg((next_block - 1) as u64)
                            .await?;
                    }
                    tracing::error!("Cumulative event hash mismatch!!");
                    tracing::error!("OPI cumulative event hash: {}", opi_cumulative_event_hash);
                    tracing::error!("Our cumulative event hash: {}", cumulative_events_hash);
                    if self.config.brc20_prog_enabled {
                        tracing::error!(
                            "OPI cumulative traces hash: {:?}",
                            opi_cumulative_trace_hash.unwrap_or_default()
                        );
                        tracing::error!("Our cumulative traces hash: {:?}", cumulative_traces_hash);
                    }
                    return Err("Cumulative hash mismatch, please check your OPI client".into());
                }
            }

            // Start reporting after 10 blocks left to full sync
            if self.config.report_to_indexer {
                let report_timer = start_timer(SPAN, "report_to_indexer", next_block);
                if let Some(last_reported_block) = self.last_reported_block {
                    if next_block >= last_reported_block - 10 || self.config.report_all_blocks {
                        self.brc20_reporter
                            .report(
                                next_block,
                                block_hash.to_string(),
                                if block_time == 0 {
                                    None
                                } else {
                                    Some(block_time)
                                },
                                block_events_hash.clone(),
                                cumulative_events_hash.clone(),
                                block_traces_hash.clone(),
                                cumulative_traces_hash.clone(),
                            )
                            .await
                            .ok();
                        self.last_reported_block = self
                            .event_provider_client
                            .get_best_verified_block()
                            .await
                            .ok(); // Try once to avoid holding up the loop
                    }
                } else {
                    self.last_reported_block = self
                        .event_provider_client
                        .get_best_verified_block()
                        .await
                        .ok(); // Try once to avoid holding up the loop
                }
                stop_timer(&report_timer).await;
            }
            stop_timer(&loop_timer).await;
        }
    }

    pub async fn validate(&mut self) -> Result<(), Box<dyn Error>> {
        let last_indexed_block_height = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;

        if self
            .validate_block(last_indexed_block_height)
            .await
            .is_err()
        {
            tracing::error!(
                "Hash mismatch found at height {}",
                last_indexed_block_height
            );
            tracing::warn!("Running a search to find the first mismatched block...");
        } else {
            tracing::info!("All blocks are valid up to {}", last_indexed_block_height);
            return Ok(());
        }

        let mut low = self.config.first_brc20_height;
        let mut high = last_indexed_block_height;
        while low <= high {
            let mid = (low + high) / 2;
            if self.validate_block(mid).await.is_err() {
                high = mid - 1;
            } else {
                low = mid + 1;
            }
        }
        if low > last_indexed_block_height {
            tracing::info!("All blocks are valid up to {}", last_indexed_block_height);
        } else {
            tracing::error!("Hash mismatch found above height {}", low);
            if last_indexed_block_height - low - 1 <= 10 {
                tracing::error!(
                    "Please reorg the indexer using `--reorg {}` and re-validate.",
                    low - 1
                )
            } else {
                tracing::error!("Please re-index from scratch using `--reset`.");
            }
        }
        Ok(())
    }

    pub async fn validate_block(&mut self, block_height: i32) -> Result<(), Box<dyn Error>> {
        tracing::warn!("Validating block {}", block_height);

        let block_data = self
            .event_provider_client
            .get_block_info_with_retries(block_height)
            .await?;

        let Some(cumulative_events_hash) = get_brc20_database()
            .lock()
            .await
            .get_cumulative_events_hash(block_height)
            .await?
        else {
            return Err(format!(
                "Cumulative events hash not found for block {}",
                block_height
            )
            .into());
        };

        tracing::warn!("Cumulative events hash: {}", cumulative_events_hash);
        tracing::warn!(
            "OPI network events hash: {}",
            block_data.best_cumulative_hash
        );

        if cumulative_events_hash != block_data.best_cumulative_hash {
            return Err(format!(
                "Cumulative events hash mismatch at block {}: expected {}, got {}",
                block_height, block_data.best_cumulative_hash, cumulative_events_hash
            )
            .into());
        }

        if block_height >= self.config.first_brc20_prog_phase_one_height {
            let Some(cumulative_trace_hash) = get_brc20_database()
                .lock()
                .await
                .get_cumulative_traces_hash(block_height)
                .await?
            else {
                return Err(format!(
                    "Cumulative traces hash not found for block {}",
                    block_height
                )
                .into());
            };

            if let Some(expected_trace_hash) = block_data.best_cumulative_trace_hash {
                tracing::warn!("Local traces hash: {}", cumulative_trace_hash);
                tracing::warn!("OPI network traces hash: {}", expected_trace_hash);
                if cumulative_trace_hash != expected_trace_hash {
                    return Err(format!(
                        "Cumulative traces hash mismatch at block {}: expected {}, got {}",
                        block_height, expected_trace_hash, cumulative_trace_hash
                    )
                    .into());
                }
            }
        }

        tracing::warn!("Block {} is valid", block_height);

        Ok(())
    }

    pub async fn get_block_event_string(
        &mut self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        get_brc20_database().lock().await.init().await?;
        let Some(block_event_str) = get_brc20_database()
            .lock()
            .await
            .get_block_events_str(block_height)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(block_event_str))
    }

    pub async fn get_block_trace_string(
        &mut self,
        block_height: i32,
    ) -> Result<Option<String>, Box<dyn Error>> {
        let Ok(Some(trace_string)) = self
            .brc20_prog_client
            .debug_get_block_trace_string(block_height.to_string())
            .await
        else {
            return Ok(None);
        };
        Ok(Some(trace_string))
    }

    pub async fn report_block(&mut self, block_height: i32) -> Result<(), Box<dyn Error>> {
        if self.config.light_client_mode {
            return Err("Reporting is not supported in light client mode".into());
        }

        let (block_hash, block_time) = self.main_db.get_block_hash_and_time(block_height).await?;

        let Some(block_events_hash) = get_brc20_database()
            .lock()
            .await
            .get_block_events_hash(block_height)
            .await?
        else {
            return Err(format!("Block events hash not found for block {}", block_height).into());
        };

        let Some(cumulative_events_hash) = get_brc20_database()
            .lock()
            .await
            .get_cumulative_events_hash(block_height)
            .await?
        else {
            return Err(format!(
                "Cumulative events hash not found for block {}",
                block_height
            )
            .into());
        };

        let cumulative_traces_hash = get_brc20_database()
            .lock()
            .await
            .get_cumulative_traces_hash(block_height)
            .await?
            .unwrap_or_default();

        let block_traces_hash = if self.config.brc20_prog_enabled
            && block_height >= self.config.first_brc20_prog_phase_one_height
        {
            retrieve_brc20_prog_traces_hash(&self.brc20_prog_client, block_height).await?
        } else {
            String::new()
        };

        self.brc20_reporter
            .report(
                block_height,
                block_hash.to_string(),
                if block_time == 0 {
                    None
                } else {
                    Some(block_time)
                },
                block_events_hash,
                cumulative_events_hash,
                block_traces_hash,
                cumulative_traces_hash,
            )
            .await?;

        Ok(())
    }

    pub async fn pre_fill_rpc_results_cache(&self, next_block: i32) -> Result<(), Box<dyn Error>> {
        if self.config.brc20_prog_enabled {
            let function_timer = start_timer(SPAN, "pre_fill_rpc_results_cache", next_block);
            let bitcoin_rpc_results = match self
                .event_provider_client
                .get_bitcoin_rpc_results_with_retries(next_block as i64)
                .await
            {
                Ok(results) => results,
                Err(error) => {
                    return Err(error);
                }
            };
            tracing::debug!(
                "Found {} Bitcoin RPC results for block {}",
                bitcoin_rpc_results.len(),
                next_block
            );
            for response in bitcoin_rpc_results {
                let request_bytes = serde_json::to_string(&response.request)?;
                let response_bytes = serde_json::to_string(&response.response)?.into();
                get_brc20_database()
                    .lock()
                    .await
                    .cache_bitcoin_rpc_request(request_bytes.as_bytes(), &response_bytes)
                    .await?;
            }
            stop_timer(&function_timer).await;
        }
        Ok(())
    }

    pub async fn finalise_block_for_brc20_prog(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        is_synced: bool,
        brc20_prog_tx_idx: u64,
    ) -> Result<(), Box<dyn Error>> {
        if self.config.brc20_prog_enabled
            && block_height >= self.config.first_brc20_prog_phase_one_height
        {
            let function_timer = start_timer(SPAN, "finalise_block_for_brc20_prog", block_height);
            self.brc20_prog_client
                .brc20_finalise_block(block_time, block_hash.try_into()?, brc20_prog_tx_idx)
                .await?;
            if is_synced || block_height % 10 == 0 {
                self.brc20_prog_client.brc20_commit_to_database().await?;
            }
            stop_timer(&function_timer).await;
        }
        Ok(())
    }

    pub async fn process_events(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        is_synced: bool,
        events: Vec<serde_json::Value>,
    ) -> Result<(), Box<dyn Error>> {
        static METHOD_SPAN: &str = "process_events";
        let function_timer = start_timer(SPAN, METHOD_SPAN, block_height);
        if events.is_empty() {
            self.finalise_block_for_brc20_prog(block_height, block_hash, block_time, is_synced, 0)
                .await?;
            stop_timer(&function_timer).await;
            return Ok(());
        }

        tracing::info!("Found {} event(s) for block {}", events.len(), block_height);

        let mut brc20_prog_tx_idx = 0;
        for event_record in events {
            let event_name = event_record
                .get("event_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!("Event type not found in event record: {:?}", event_record)
                })?;
            let single_event_timer = start_timer(METHOD_SPAN, event_name, block_height);
            let event_type_id = event_name_to_id(&event_name);
            let inscription_id = event_record
                .get("inscription_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "Inscription ID not found in event record: {:?}",
                        event_record
                    )
                })?;
            if event_type_id == Brc20ProgDeployInscribeEvent::event_id() {
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut load_event::<Brc20ProgDeployInscribeEvent>(event_type_id, &event_record)?,
                    0,
                )?;
                EventProcessor::brc20_prog_deploy_inscribe(block_height, &inscription_id).await?;
            } else if event_type_id == Brc20ProgDeployTransferEvent::event_id() {
                let mut event =
                    load_event::<Brc20ProgDeployTransferEvent>(event_type_id, &event_record)?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut event,
                    0,
                )?;
                match EventProcessor::brc20_prog_deploy_transfer(
                    &self.brc20_prog_client,
                    block_height,
                    block_time,
                    block_hash,
                    brc20_prog_tx_idx,
                    &inscription_id,
                    &event,
                )
                .await
                {
                    Ok(tx_executed) => {
                        brc20_prog_tx_idx += tx_executed.count;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process Brc20ProgDeployTransferEvent: {}", e);
                        return Err(e.into());
                    }
                }
            } else if event_type_id == Brc20ProgCallInscribeEvent::event_id() {
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut load_event::<Brc20ProgCallInscribeEvent>(event_type_id, &event_record)?,
                    0,
                )?;
                EventProcessor::brc20_prog_call_inscribe(block_height, &inscription_id).await?;
            } else if event_type_id == Brc20ProgCallTransferEvent::event_id() {
                let mut event =
                    load_event::<Brc20ProgCallTransferEvent>(event_type_id, &event_record)?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut event,
                    0,
                )?;
                match EventProcessor::brc20_prog_call_transfer(
                    &self.brc20_prog_client,
                    block_height,
                    block_time,
                    block_hash,
                    brc20_prog_tx_idx,
                    &inscription_id,
                    &mut event,
                )
                .await
                {
                    Ok(tx_executed) => {
                        brc20_prog_tx_idx += tx_executed.count;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process Brc20ProgCallTransferEvent: {}", e);
                        return Err(e.into());
                    }
                }
            } else if event_type_id == Brc20ProgTransactInscribeEvent::event_id() {
                let mut event =
                    load_event::<Brc20ProgTransactInscribeEvent>(event_type_id, &event_record)?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut event,
                    0,
                )?;
                EventProcessor::brc20_prog_transact_inscribe(block_height, &inscription_id).await?;
            } else if event_type_id == Brc20ProgTransactTransferEvent::event_id() {
                let mut event =
                    load_event::<Brc20ProgTransactTransferEvent>(event_type_id, &event_record)?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut event,
                    0,
                )?;
                match EventProcessor::brc20_prog_transact_transfer(
                    &self.brc20_prog_client,
                    block_height,
                    block_hash,
                    &inscription_id,
                    block_time,
                    brc20_prog_tx_idx,
                    &event,
                )
                .await
                {
                    Ok(txes_executed) => {
                        brc20_prog_tx_idx += txes_executed.count;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process Brc20ProgTransactTransferEvent: {}", e);
                        return Err(e.into());
                    }
                }
            } else if event_type_id == Brc20ProgWithdrawInscribeEvent::event_id() {
                let event =
                    load_event::<Brc20ProgWithdrawInscribeEvent>(event_type_id, &event_record)?;
                let ticker = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                    .ok_or_else(|| {
                        format!(
                            "Ticker {} not found for Brc20ProgWithdrawInscribeEvent",
                            event.ticker
                        )
                    })?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut load_event::<Brc20ProgWithdrawInscribeEvent>(
                        event_type_id,
                        &event_record,
                    )?,
                    ticker.decimals,
                )?;
                EventProcessor::brc20_prog_withdraw_inscribe(block_height, &inscription_id).await?;
            } else if event_type_id == Brc20ProgWithdrawTransferEvent::event_id() {
                let mut event =
                    load_event::<Brc20ProgWithdrawTransferEvent>(event_type_id, &event_record)?;
                let ticker = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                    .ok_or_else(|| {
                        format!(
                            "Ticker {} not found for Brc20ProgWithdrawTransferEvent",
                            event.ticker
                        )
                    })?;
                let event_id = get_brc20_database().lock().await.add_light_event(
                    block_height,
                    inscription_id,
                    &mut event,
                    ticker.decimals,
                )?;
                match EventProcessor::brc20_prog_withdraw_transfer(
                    &self.brc20_prog_client,
                    block_height,
                    block_hash,
                    block_time,
                    brc20_prog_tx_idx,
                    &inscription_id,
                    event_id,
                    &event,
                )
                .await
                {
                    Ok(tx_executed) => {
                        brc20_prog_tx_idx += tx_executed.count;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process Brc20ProgWithdrawTransferEvent: {}", e);
                        return Err(e.into());
                    }
                }
            } else if event_type_id == DeployInscribeEvent::event_id() {
                let mut event = load_event::<DeployInscribeEvent>(event_type_id, &event_record)?;
                let decimals = event.decimals;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    &inscription_id,
                    &mut event,
                    decimals,
                )?;
                EventProcessor::brc20_deploy_inscribe(block_height, &inscription_id, &event)
                    .await?;
            } else if event_type_id == MintInscribeEvent::event_id() {
                let mut event = load_event::<MintInscribeEvent>(event_type_id, &event_record)?;
                let ticker = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                    .ok_or_else(|| {
                        format!("Ticker {} not found for MintInscribeEvent", event.ticker)
                    })?;
                let event_id = get_brc20_database().lock().await.add_light_event(
                    block_height,
                    &inscription_id,
                    &mut event,
                    ticker.decimals,
                )?;
                EventProcessor::brc20_mint_inscribe(block_height, event_id, &event).await?;
            } else if event_type_id == PreDeployInscribeEvent::event_id() {
                let mut event = load_event::<PreDeployInscribeEvent>(event_type_id, &event_record)?;
                get_brc20_database().lock().await.add_light_event(
                    block_height,
                    &inscription_id,
                    &mut event,
                    0,
                )?;
                // PreDeployInscribeEvent is not processed here, it is handled already in the transfer processing
            } else if event_type_id == TransferInscribeEvent::event_id() {
                let mut event = load_event::<TransferInscribeEvent>(event_type_id, &event_record)?;
                let ticker = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                    .ok_or_else(|| {
                        format!(
                            "Ticker {} not found for TransferInscribeEvent",
                            event.ticker
                        )
                    })?;
                let event_id = get_brc20_database().lock().await.add_light_event(
                    block_height,
                    &inscription_id,
                    &mut event,
                    ticker.decimals,
                )?;
                EventProcessor::brc20_transfer_inscribe(
                    block_height,
                    event_id,
                    &inscription_id,
                    &event,
                )
                .await?;
            } else if event_type_id == TransferTransferEvent::event_id() {
                let mut event = load_event::<TransferTransferEvent>(event_type_id, &event_record)?;
                let ticker = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                    .ok_or_else(|| {
                        format!(
                            "Ticker {} not found for TransferTransferEvent",
                            event.ticker
                        )
                    })?;
                let event_id = get_brc20_database().lock().await.add_light_event(
                    block_height,
                    &inscription_id,
                    &mut event,
                    ticker.decimals,
                )?;
                match EventProcessor::brc20_transfer_transfer(
                    &self.brc20_prog_client,
                    block_height,
                    block_time,
                    block_hash,
                    brc20_prog_tx_idx,
                    &inscription_id,
                    event_id,
                    &event,
                    &self.config,
                )
                .await
                {
                    Ok(tx_executed) => {
                        brc20_prog_tx_idx += tx_executed.count;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process TransferTransferEvent: {}", e);
                        return Err(e.into());
                    }
                }
            } else {
                tracing::warn!("Unknown event type: {}", event_type_id);
                return Err(format!(
                    "Unknown event type: {} for block {}",
                    event_type_id, block_height
                )
                .into());
            }
            stop_timer(&single_event_timer).await;
        }

        let flush_timer = start_timer(SPAN, "flush_queries_to_db", block_height);
        get_brc20_database()
            .lock()
            .await
            .flush_queries_to_db()
            .await?;
        stop_timer(&flush_timer).await;

        self.finalise_block_for_brc20_prog(
            block_height,
            block_hash,
            block_time,
            is_synced,
            brc20_prog_tx_idx,
        )
        .await?;

        stop_timer(&function_timer).await;
        Ok(())
    }

    /// Generates events for the given block height.
    pub async fn generate_and_process_events(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        is_synced: bool,
    ) -> Result<(), Box<dyn Error>> {
        static METHOD_SPAN: &str = "generate_and_process_events";
        let function_timer = start_timer(SPAN, METHOD_SPAN, block_height);
        let mut brc20_prog_tx_idx: u64 = 0;

        if block_height == self.config.brc20_swap_refund_activation_height {
            Brc20SwapRefund::generate_and_process_refunds(
                block_height,
                &self.brc20_prog_client,
                &self.config,
                block_time,
                block_hash,
            )
            .await?;
        }

        let get_transfers_timer = start_timer(SPAN, "get_transfers", block_height);
        let transfers = self.main_db.get_transfers(block_height).await?;
        stop_timer(&get_transfers_timer).await;
        if transfers.is_empty() {
            self.finalise_block_for_brc20_prog(block_height, block_hash, block_time, is_synced, 0)
                .await?;
            return Ok(());
        }

        tracing::info!(
            "Found {} transfer(s) for block {}",
            transfers.len(),
            block_height,
        );

        let mut last_transfer_timer = None;
        for index in 0..transfers.len() {
            last_transfer_timer = match last_transfer_timer {
                Some(timer) => {
                    stop_timer(&timer).await;
                    Some(start_timer(METHOD_SPAN, format!("transfer"), block_height))
                }
                None => Some(start_timer(METHOD_SPAN, format!("transfer"), block_height)),
            };
            let transfer = &transfers[index];
            if index % 100 == 0 && transfers.len() > 100 {
                tracing::debug!("Processing transfer {} / {}", index, transfers.len());
            }

            if transfer.sent_as_fee && transfer.old_satpoint.as_ref().is_none_or(|x| x.is_empty()) {
                tracing::debug!(
                    "Skipping transfer {} as it is sent as fee and old satpoint is not present",
                    transfer.inscription_id
                );
                continue;
            }

            let Some(protocol) = transfer.content.get(PROTOCOL_KEY).and_then(|p| p.as_str()) else {
                tracing::debug!(
                    "Skipping transfer {} as protocol is not present",
                    transfer.inscription_id
                );
                continue;
            };

            if protocol != PROTOCOL_BRC20
                && protocol != PROTOCOL_BRC20_PROG
                && protocol != PROTOCOL_BRC20_MODULE
            {
                tracing::debug!(
                    "Skipping transfer {} as protocol is not BRC20 or BRC20 Prog",
                    transfer.inscription_id
                );
                continue;
            }

            let Some(operation) = transfer
                .content
                .get(OPERATION_KEY)
                .and_then(|op| op.as_str())
            else {
                tracing::debug!(
                    "Skipping transfer {} as operation is not present",
                    transfer.inscription_id
                );
                continue;
            };

            if protocol == PROTOCOL_BRC20_PROG {
                if !self.config.brc20_prog_enabled {
                    tracing::debug!(
                        "Skipping transfer {} as BRC20 Prog is not enabled",
                        transfer.inscription_id
                    );
                    continue;
                }

                if block_height < self.config.first_brc20_prog_phase_one_height {
                    tracing::debug!(
                        "Skipping transfer {} as block height {} is less than first BRC20 Prog height {}",
                        transfer.inscription_id,
                        block_height,
                        self.config.first_brc20_prog_phase_one_height
                    );
                    continue;
                }

                let data = transfer.content.get(DATA_KEY).and_then(|d| d.as_str());
                let base64_data = transfer
                    .content
                    .get(BASE64_DATA_KEY)
                    .and_then(|b| b.as_str());

                if data.is_none() && base64_data.is_none() {
                    tracing::debug!(
                        "Skipping transfer {} as data or base64_data is not present",
                        transfer.inscription_id
                    );
                    continue;
                }

                if data.is_some() && base64_data.is_some() {
                    tracing::debug!(
                        "Skipping transfer {} as both data and base64_data are present",
                        transfer.inscription_id
                    );
                    continue;
                }

                if operation == OPERATION_BRC20_PROG_DEPLOY
                    || operation == OPERATION_BRC20_PROG_DEPLOY_SHORT
                {
                    if transfer.old_satpoint.is_some() {
                        match EventGenerator::brc20_prog_deploy_transfer(
                            block_height,
                            block_height >= self.config.first_brc20_prog_prague_height,
                            data,
                            base64_data,
                            transfer,
                        )
                        .await
                        {
                            Ok(event) => {
                                match EventProcessor::brc20_prog_deploy_transfer(
                                    &self.brc20_prog_client,
                                    block_height,
                                    block_time,
                                    block_hash,
                                    brc20_prog_tx_idx,
                                    &transfer.inscription_id,
                                    &event,
                                )
                                .await
                                {
                                    Ok(tx_executed) => {
                                        brc20_prog_tx_idx += tx_executed.count;
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to process BRC20 Prog deploy transfer event: {}",
                                            e
                                        );
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Failed to generate BRC20 Prog deploy transfer event: {}",
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        EventGenerator::brc20_prog_deploy_inscribe(
                            block_height,
                            data,
                            base64_data,
                            transfer,
                        )
                        .await?;
                        EventProcessor::brc20_prog_deploy_inscribe(
                            block_height,
                            &transfer.inscription_id,
                        )
                        .await?;
                    }
                } else if operation == OPERATION_BRC20_PROG_CALL
                    || operation == OPERATION_BRC20_PROG_CALL_SHORT
                {
                    if transfer.content.get(CONTRACT_ADDRESS_KEY).is_none()
                        && transfer.content.get(INSCRIPTION_ID_KEY).is_none()
                    {
                        tracing::debug!(
                            "Skipping transfer {} as contract address or inscription ID is not present",
                            transfer.inscription_id
                        );
                        continue;
                    }
                    if transfer.old_satpoint.is_some() {
                        match EventGenerator::brc20_prog_call_transfer(
                            block_height,
                            block_height >= self.config.first_brc20_prog_prague_height,
                            transfer
                                .content
                                .get(CONTRACT_ADDRESS_KEY)
                                .and_then(|c| c.as_str()),
                            transfer
                                .content
                                .get(INSCRIPTION_ID_KEY)
                                .and_then(|i| i.as_str()),
                            data,
                            base64_data,
                            transfer,
                        )
                        .await
                        {
                            Ok(event) => {
                                match EventProcessor::brc20_prog_call_transfer(
                                    &self.brc20_prog_client,
                                    block_height,
                                    block_time,
                                    block_hash,
                                    brc20_prog_tx_idx,
                                    &transfer.inscription_id,
                                    &event,
                                )
                                .await
                                {
                                    Ok(tx_executed) => {
                                        brc20_prog_tx_idx += tx_executed.count;
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to process BRC20 Prog call transfer event: {}",
                                            e
                                        );
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to generate BRC20 Prog call transfer event: {}",
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        EventGenerator::brc20_prog_call_inscribe(
                            block_height,
                            transfer
                                .content
                                .get(CONTRACT_ADDRESS_KEY)
                                .and_then(|c| c.as_str()),
                            transfer
                                .content
                                .get(INSCRIPTION_ID_KEY)
                                .and_then(|i| i.as_str()),
                            data,
                            base64_data,
                            transfer,
                        )
                        .await?;
                        EventProcessor::brc20_prog_call_inscribe(
                            block_height,
                            &transfer.inscription_id,
                        )
                        .await?;
                    }
                } else if operation == OPERATION_BRC20_PROG_TRANSACT
                    || operation == OPERATION_BRC20_PROG_TRANSACT_SHORT
                {
                    if transfer.old_satpoint.is_some() {
                        match EventGenerator::brc20_prog_transact_transfer(
                            block_height,
                            block_height >= self.config.first_brc20_prog_prague_height,
                            data,
                            base64_data,
                            transfer,
                        )
                        .await
                        {
                            Ok(event) => {
                                match EventProcessor::brc20_prog_transact_transfer(
                                    &self.brc20_prog_client,
                                    block_height,
                                    block_hash,
                                    &transfer.inscription_id,
                                    block_time,
                                    brc20_prog_tx_idx,
                                    &event,
                                )
                                .await
                                {
                                    Ok(txes_executed) => {
                                        brc20_prog_tx_idx += txes_executed.count;
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Failed to generate BRC20 Prog transact transfer event: {}",
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        EventGenerator::brc20_prog_transact_inscribe(
                            block_height,
                            data,
                            base64_data,
                            transfer,
                        )
                        .await?;
                        EventProcessor::brc20_prog_transact_inscribe(
                            block_height,
                            &transfer.inscription_id,
                        )
                        .await?;
                    }
                }
                continue;
            }

            if operation == OPERATION_PREDEPLOY && transfer.old_satpoint.is_none() {
                if block_height
                    < self.config.first_brc20_prog_phase_one_height
                        - PREDEPLOY_BLOCK_HEIGHT_ACCEPTANCE_DELAY
                {
                    tracing::debug!(
                        "Skipping transfer {} as block height {} is too early",
                        transfer.inscription_id,
                        block_height
                    );
                    continue;
                }

                let Some(hash) = transfer.content.get(HASH_KEY).and_then(|h| h.as_str()) else {
                    tracing::debug!(
                        "Skipping transfer {} as hash is not present",
                        transfer.inscription_id
                    );
                    continue;
                };

                EventGenerator::brc20_predeploy_inscribe(block_height, hash, &transfer).await?;
                continue;
            }

            let Some(original_ticker) = transfer.content.get(TICKER_KEY).and_then(|ot| ot.as_str())
            else {
                tracing::debug!(
                    "Skipping transfer {} as ticker is not present",
                    transfer.inscription_id
                );
                continue;
            };

            let ticker = original_ticker.to_lowercase();

            // if ticker or original_ticker contains \x00, skip the transfer
            if ticker.contains('\x00') || original_ticker.contains('\x00') {
                tracing::debug!(
                    "Skipping transfer {} as ticker or original_ticker contains null byte",
                    transfer.inscription_id
                );
                continue;
            }

            let ticker_length = original_ticker.as_bytes().len();

            if !(ticker_length == 4
                || ticker_length == 5
                || (ticker_length == 6
                    && is_alphanumerical_or_dash(&ticker)
                    && block_height >= self.config.first_brc20_prog_phase_one_height))
            {
                tracing::debug!(
                    "Skipping transfer {} as ticker length {} is not valid at block height {}",
                    transfer.inscription_id,
                    ticker_length,
                    block_height
                );
                continue;
            }

            if protocol == PROTOCOL_BRC20_MODULE {
                let Ok(Some(deployed_ticker)) =
                    get_brc20_database().lock().await.get_ticker(&ticker)
                else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Some(module) = transfer.content.get(MODULE_KEY).and_then(|m| m.as_str()) else {
                    tracing::debug!(
                        "Skipping transfer {} as module is not present",
                        transfer.inscription_id
                    );
                    continue;
                };

                if module != BRC20_MODULE_BRC20PROG
                    || !self.config.brc20_prog_enabled
                    || operation != OPERATION_WITHDRAW
                {
                    tracing::debug!(
                        "Skipping transfer {} as module is not BRC20 Prog or operation is not withdraw",
                        transfer.inscription_id
                    );
                    continue;
                }

                let Ok(amount) = get_amount_value(
                    transfer.content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
                    deployed_ticker.decimals,
                    no_default!(),
                    DISALLOW_ZERO,
                ) else {
                    tracing::debug!(
                        "Skipping transfer {} as amount is not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                if let Some(_) = transfer.old_satpoint.as_ref() {
                    let (event_id, event) = EventGenerator::brc20_prog_withdraw_transfer(
                        block_height,
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        transfer,
                    )
                    .await?;
                    match EventProcessor::brc20_prog_withdraw_transfer(
                        &self.brc20_prog_client,
                        block_height,
                        block_hash,
                        block_time,
                        brc20_prog_tx_idx,
                        &transfer.inscription_id,
                        event_id,
                        &event,
                    )
                    .await
                    {
                        Ok(tx_executed) => {
                            brc20_prog_tx_idx += tx_executed.count;
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to process Brc20ProgWithdrawTransferEvent: {}",
                                e
                            );
                            return Err(e.into());
                        }
                    }
                } else {
                    EventGenerator::brc20_prog_withdraw_inscribe(
                        block_height,
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        transfer,
                    )
                    .await?;
                    EventProcessor::brc20_prog_withdraw_inscribe(
                        block_height,
                        &transfer.inscription_id,
                    )
                    .await?;
                }
                continue;
            }

            if operation == OPERATION_DEPLOY && transfer.old_satpoint.is_none() {
                if let Ok(Some(_)) = get_brc20_database().lock().await.get_ticker(&ticker) {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is already deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                }

                let Ok(decimals) =
                    get_decimals_value(transfer.content.get(DECIMALS_KEY).and_then(|d| d.as_str()))
                else {
                    tracing::debug!(
                        "Skipping transfer {} as decimals are not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                let Ok(mut max_supply) = get_amount_value(
                    transfer
                        .content
                        .get(MAX_SUPPLY_KEY)
                        .and_then(|m| m.as_str()),
                    decimals,
                    no_default!(),
                    ALLOW_ZERO,
                ) else {
                    tracing::debug!(
                        "Skipping transfer {} as max supply is not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                let mut limit_per_mint_res = get_amount_value(
                    transfer
                        .content
                        .get(LIMIT_PER_MINT_KEY)
                        .and_then(|l| l.as_str()),
                    decimals,
                    no_default!(),
                    DISALLOW_ZERO,
                );

                if limit_per_mint_res.is_err() {
                    if transfer.content.get(LIMIT_PER_MINT_KEY).is_none() {
                        limit_per_mint_res = Ok(max_supply);
                    } else {
                        tracing::debug!(
                            "Skipping transfer {} as limit per mint is not present or invalid",
                            transfer.inscription_id
                        );
                        continue;
                    }
                }

                let mut limit_per_mint = limit_per_mint_res?;

                let mut is_self_mint = false;
                if ticker_length == 5 {
                    if block_height < self.config.self_mint_activation_height {
                        tracing::debug!(
                            "Skipping transfer {} as self mint is not enabled yet",
                            transfer.inscription_id
                        );
                        continue;
                    }
                    if let Some(self_mint) =
                        transfer.content.get(SELF_MINT_KEY).and_then(|s| s.as_str())
                    {
                        if self_mint != "true" {
                            tracing::debug!(
                                "Skipping transfer {} as self mint is not enabled",
                                transfer.inscription_id
                            );
                            continue;
                        }
                    } else {
                        tracing::debug!(
                            "Skipping transfer {} as self mint is not present",
                            transfer.inscription_id
                        );
                        continue;
                    }
                    is_self_mint = true;
                    if max_supply == 0 {
                        max_supply = MAX_AMOUNT;
                        if limit_per_mint == 0 {
                            limit_per_mint = MAX_AMOUNT;
                        }
                    }
                } else if ticker_length == 6 {
                    if block_height < self.config.first_brc20_prog_phase_one_height {
                        tracing::debug!(
                            "Skipping transfer {} as 6-byte tickers are not enabled yet",
                            transfer.inscription_id
                        );
                        continue;
                    }

                    let Some(salt) = transfer.content.get(SALT_KEY).and_then(|s| s.as_str()) else {
                        tracing::debug!(
                            "Skipping transfer {} as salt is not present or invalid",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    let Some(parent_id) = transfer.parent_id.as_ref() else {
                        tracing::debug!(
                            "Skipping transfer {} as parent ID is not present",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    let Some(predeploy_event) = get_brc20_database()
                        .lock()
                        .await
                        .get_event_with_type::<PreDeployInscribeEvent>(parent_id)
                        .await?
                    else {
                        tracing::debug!(
                            "Skipping transfer {} as predeploy event is not present",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    if predeploy_event.block_height > block_height - PREDEPLOY_BLOCK_HEIGHT_DELAY {
                        tracing::debug!(
                            "Skipping transfer {} as predeploy block height {} is too recent",
                            transfer.inscription_id,
                            predeploy_event.block_height
                        );
                        continue;
                    }

                    let Ok(salt_bytes) = hex::decode(salt) else {
                        tracing::debug!(
                            "Skipping transfer {} as salt is not a valid hex string",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    let Ok(pkscript_bytes) =
                        hex::decode(predeploy_event.predeployer_pk_script.as_str())
                    else {
                        tracing::debug!(
                            "Skipping transfer {} as pkscript is not a valid hex string",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    let salted_ticker =
                        [original_ticker.as_bytes(), &salt_bytes, &pkscript_bytes].concat();

                    if predeploy_event.hash
                        != sha256::digest(hex::decode(sha256::digest(&salted_ticker))?)
                    {
                        tracing::debug!(
                            "Skipping transfer {} as ticker hash does not match predeploy hash",
                            transfer.inscription_id
                        );
                        continue;
                    }

                    if let Some(self_mint) =
                        transfer.content.get(SELF_MINT_KEY).and_then(|s| s.as_str())
                    {
                        is_self_mint = self_mint == "true";
                    } else {
                        is_self_mint = false;
                    }
                    if is_self_mint {
                        if max_supply == 0 {
                            max_supply = MAX_AMOUNT;
                            if limit_per_mint == 0 {
                                limit_per_mint = MAX_AMOUNT;
                            }
                        }
                    }
                }

                if max_supply == 0 {
                    tracing::debug!(
                        "Skipping transfer {} as max supply is 0",
                        transfer.inscription_id
                    );
                    continue;
                }

                let event = EventGenerator::brc20_deploy_inscribe(
                    block_height,
                    ticker.as_str(),
                    original_ticker,
                    max_supply,
                    limit_per_mint,
                    decimals,
                    is_self_mint,
                    transfer,
                )
                .await?;
                EventProcessor::brc20_deploy_inscribe(
                    block_height,
                    &transfer.inscription_id,
                    &event,
                )
                .await?;

                continue;
            }

            if operation == OPERATION_MINT && transfer.old_satpoint.is_none() {
                let Ok(Some(mut deployed_ticker)) =
                    get_brc20_database().lock().await.get_ticker(&ticker)
                else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Ok(amount) = get_amount_value(
                    transfer.content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
                    deployed_ticker.decimals,
                    no_default!(),
                    DISALLOW_ZERO,
                ) else {
                    tracing::debug!(
                        "Skipping transfer {} as amount is not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                match EventGenerator::brc20_mint_inscribe(
                    block_height,
                    &mut deployed_ticker,
                    original_ticker,
                    amount,
                    transfer,
                )
                .await
                {
                    Ok((event_id, event)) => {
                        EventProcessor::brc20_mint_inscribe(block_height, event_id, &event).await?;
                    }
                    Err(_) => {
                        tracing::debug!(
                            "Failed to generate BRC20 mint inscribe event for transfer {}",
                            transfer.inscription_id
                        );
                    }
                }
                continue;
            }

            if operation == OPERATION_TRANSFER {
                let Ok(Some(mut deployed_ticker)) =
                    get_brc20_database().lock().await.get_ticker(&ticker)
                else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Ok(amount) = get_amount_value(
                    transfer.content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
                    deployed_ticker.decimals,
                    no_default!(),
                    DISALLOW_ZERO,
                ) else {
                    tracing::debug!(
                        "Skipping transfer {} as amount is not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                if transfer.old_satpoint.is_some() {
                    match EventGenerator::brc20_transfer_transfer(
                        block_height,
                        &mut deployed_ticker,
                        original_ticker,
                        amount,
                        transfer,
                    )
                    .await
                    {
                        Ok((event_id, event)) => {
                            match EventProcessor::brc20_transfer_transfer(
                                &self.brc20_prog_client,
                                block_height,
                                block_time,
                                block_hash,
                                brc20_prog_tx_idx,
                                &transfer.inscription_id,
                                event_id,
                                &event,
                                &self.config,
                            )
                            .await
                            {
                                Ok(txes_executed) => {
                                    brc20_prog_tx_idx += txes_executed.count;
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "Failed to process Brc20TransferTransferEvent: {}",
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Failed to generate BRC20 transfer transfer event for transfer {}: {}",
                                transfer.inscription_id,
                                e
                            );
                            continue;
                        }
                    }
                } else {
                    match EventGenerator::brc20_transfer_inscribe(
                        block_height,
                        &mut deployed_ticker,
                        original_ticker,
                        amount,
                        transfer,
                    )
                    .await
                    {
                        Ok((event_id, event)) => {
                            EventProcessor::brc20_transfer_inscribe(
                                block_height,
                                event_id,
                                &transfer.inscription_id,
                                &event,
                            )
                            .await?;
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Failed to generate BRC20 transfer inscribe event for transfer {}: {}",
                                transfer.inscription_id,
                                e
                            );
                        }
                    }
                }
            }
            continue;
        }
        if let Some(timer) = last_transfer_timer {
            stop_timer(&timer).await;
        }

        let flush_timer = start_timer(SPAN, "flush_queries_to_db", block_height);
        get_brc20_database()
            .lock()
            .await
            .flush_queries_to_db()
            .await?;
        stop_timer(&flush_timer).await;

        self.finalise_block_for_brc20_prog(
            block_height,
            block_hash,
            block_time,
            is_synced,
            brc20_prog_tx_idx,
        )
        .await?;

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn get_opi_block_height(&self) -> Result<i32, Box<dyn Error>> {
        let result = if self.config.light_client_mode {
            self.event_provider_client
                .get_best_verified_block_with_retries()
                .await
        } else {
            self.main_db.get_current_block_height().await
        };
        result
    }

    pub async fn reorg_to_last_synced_block_height(&mut self) -> Result<(), Box<dyn Error>> {
        tracing::debug!("Reorganizing BRC20 indexer to last synced block height...");
        let mut last_brc20_block_height = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;
        let last_opi_block_height = self.last_opi_block;
        let mut last_brc20_prog_block_height = if !self.config.brc20_prog_enabled
            || last_brc20_block_height < self.config.first_brc20_prog_phase_one_height
        {
            self.config.first_brc20_prog_phase_one_height - 1
        } else {
            parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?
        };

        // BRC20 indexer has not indexed any blocks yet, no need to reorg BRC20
        if last_brc20_block_height < self.config.first_brc20_height {
            if self.config.brc20_prog_enabled
                && last_brc20_prog_block_height > self.config.first_brc20_prog_phase_one_height
            {
                self.brc20_prog_client
                    .brc20_reorg(self.config.first_brc20_prog_phase_one_height as u64)
                    .await?;
            }
            return Ok(());
        }

        tracing::debug!(
            "BRC20 indexer last block height: {}, OPI indexer last block height: {}, BRC20 Prog last block height: {}",
            last_brc20_block_height,
            last_opi_block_height,
            last_brc20_prog_block_height
        );

        if self.config.brc20_prog_enabled
            && last_brc20_prog_block_height >= self.config.first_brc20_prog_phase_one_height
        {
            if last_brc20_block_height == last_brc20_prog_block_height {
                // BRC20 and BRC20 Prog are already synced, no need to reorg
                tracing::debug!(
                    "BRC20 and BRC20 Prog are synced at height: {}",
                    last_brc20_block_height
                );
            } else if last_brc20_block_height < last_brc20_prog_block_height {
                tracing::info!(
                    "BRC20 Prog is ahead of BRC20, reorging BRC20 prog to current BRC20 height: {}",
                    last_brc20_block_height
                );
                self.brc20_prog_client
                    .brc20_reorg(last_brc20_block_height as u64)
                    .await?;

                last_brc20_prog_block_height = last_brc20_block_height;
            } else {
                tracing::info!(
                    "BRC20 is ahead of BRC20 Prog, reorging BRC20 to current BRC20 prog height: {}",
                    last_brc20_prog_block_height
                );
                get_brc20_database()
                    .lock()
                    .await
                    .reorg(last_brc20_prog_block_height as i32)
                    .await?;

                last_brc20_block_height = last_brc20_prog_block_height;
            }
        }

        let mut current_brc20_height = last_brc20_block_height;
        let mut current_brc20_prog_height = last_brc20_prog_block_height;
        loop {
            if current_brc20_height < last_brc20_block_height - 10 {
                // If the difference is greater than 10, we don't support reorg
                return Err(format!(
                    "BRC20 REORG IS TOO LARGE, LAST BLOCK HEIGHT: {}",
                    last_brc20_block_height
                )
                .into());
            }

            let brc20_block_hash = get_brc20_database()
                .lock()
                .await
                .get_block_hash(current_brc20_height)
                .await?;
            let opi_block_hash = if self.config.light_client_mode {
                self.event_provider_client
                    .get_block_info_with_retries(current_brc20_height)
                    .await?
                    .best_block_hash
            } else {
                self.main_db.get_block_hash(current_brc20_height).await?
            };
            let brc20_prog_block_hash = if !self.config.brc20_prog_enabled
                || current_brc20_height < self.config.first_brc20_prog_phase_one_height
            {
                opi_block_hash.clone()
            } else {
                self.brc20_prog_client
                    .eth_get_block_by_number(current_brc20_height.to_string(), Some(false))
                    .await?
                    .hash
                    .bytes
                    .to_string()
                    .trim_start_matches("0x")
                    .to_string()
            };

            tracing::debug!(
                "Checking block height {}, BRC20 hash: {}, OPI hash: {}, BRC20 Prog hash: {}",
                current_brc20_height,
                brc20_block_hash,
                opi_block_hash,
                brc20_prog_block_hash
            );

            if brc20_block_hash == opi_block_hash && brc20_block_hash == brc20_prog_block_hash {
                // We found a block hash that is the same in all three databases
                tracing::debug!(
                    "Found synced block at height {}, hash: {}, last BRC20 block height: {}",
                    current_brc20_height,
                    brc20_block_hash,
                    last_brc20_block_height
                );
                if last_brc20_block_height == current_brc20_height {
                    // Last block is synced, no need to reorg
                    break;
                }
                get_brc20_database()
                    .lock()
                    .await
                    .reorg(current_brc20_height)
                    .await?;
                if self.config.brc20_prog_enabled
                    && current_brc20_height >= self.config.first_brc20_prog_phase_one_height
                {
                    self.brc20_prog_client
                        .brc20_reorg(current_brc20_prog_height as u64)
                        .await?;
                }
                break;
            }

            if current_brc20_height == self.config.first_brc20_height {
                tracing::debug!(
                    "Reached first BRC20 height {}, reorging to it",
                    self.config.first_brc20_height
                );
                // We reached the first inscription height, reorg everyone to their first heights
                get_brc20_database()
                    .lock()
                    .await
                    .reorg(self.config.first_brc20_height)
                    .await?;
                if self.config.brc20_prog_enabled {
                    self.brc20_prog_client
                        .brc20_reorg(self.config.first_brc20_prog_phase_one_height as u64)
                        .await?;
                }
                break;
            }
            current_brc20_height -= 1;
            current_brc20_prog_height -= 1;
        }

        Ok(())
    }
}

fn is_alphanumerical_or_dash(ticker: &str) -> bool {
    ticker
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn parse_hex_number(eth_number: &str) -> Result<i32, Box<dyn Error>> {
    i32::from_str_radix(eth_number.trim_start_matches("0x"), 16).map_err(|e| e.into())
}
