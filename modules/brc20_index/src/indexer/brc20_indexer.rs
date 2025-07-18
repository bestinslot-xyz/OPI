use std::error::Error;

use brc20_prog::{
    Brc20ProgApiClient,
    types::{Base64Bytes, RawBytes},
};
use db_reader::BRC20Tx;
use jsonrpsee::http_client::HttpClient;
use tokio::task::JoinHandle;

use crate::{
    config::{
        AMOUNT_KEY, BASE64_DATA_KEY, BRC20_MODULE_BRC20PROG, BRC20_PROG_OP_RETURN_PKSCRIPT,
        BRC20_PROG_VERSION, Brc20IndexerConfig, CONTRACT_ADDRESS_KEY, DATA_KEY, DB_VERSION,
        DECIMALS_KEY, EVENT_SEPARATOR, HASH_KEY, INSCRIPTION_ID_KEY, LIMIT_PER_MINT_KEY,
        MAX_AMOUNT, MAX_SUPPLY_KEY, MODULE_KEY, NO_WALLET, OP_RETURN, OPERATION_BRC20_PROG_CALL,
        OPERATION_BRC20_PROG_CALL_SHORT, OPERATION_BRC20_PROG_DEPLOY,
        OPERATION_BRC20_PROG_DEPLOY_SHORT, OPERATION_BRC20_PROG_TRANSACT,
        OPERATION_BRC20_PROG_TRANSACT_SHORT, OPERATION_DEPLOY, OPERATION_KEY, OPERATION_MINT,
        OPERATION_PREDEPLOY, OPERATION_TRANSFER, OPERATION_WITHDRAW, PREDEPLOY_BLOCK_HEIGHT_DELAY,
        PROTOCOL_BRC20, PROTOCOL_BRC20_MODULE, PROTOCOL_BRC20_PROG, PROTOCOL_KEY, SALT_KEY,
        SELF_MINT_ENABLE_HEIGHT, SELF_MINT_KEY, TICKER_KEY,
    },
    database::{Brc20Balance, OpiDatabase, TransferValidity, get_brc20_database},
    indexer::{
        brc20_prog_balance_server::run_balance_server,
        brc20_prog_client::build_brc20_prog_http_client,
        brc20_reporter::Brc20Reporter,
        utils::{ALLOW_ZERO, DISALLOW_ZERO, get_amount_value, get_decimals_value},
    },
    no_default,
    types::{
        Ticker,
        events::{
            Brc20ProgCallInscribeEvent, Brc20ProgCallTransferEvent, Brc20ProgDeployInscribeEvent,
            Brc20ProgDeployTransferEvent, Brc20ProgTransactInscribeEvent,
            Brc20ProgTransactTransferEvent, Brc20ProgWithdrawInscribeEvent,
            Brc20ProgWithdrawTransferEvent, DeployInscribeEvent, Event, MintInscribeEvent,
            PreDeployInscribeEvent, TransferInscribeEvent, TransferTransferEvent,
        },
    },
};

pub struct Brc20Indexer {
    main_db: OpiDatabase,
    config: Brc20IndexerConfig,
    brc20_prog_client: HttpClient,
    brc20_reporter: Brc20Reporter,
    server_handle: Option<JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>>,
}

impl Brc20Indexer {
    pub fn new(config: Brc20IndexerConfig) -> Self {
        let main_db = OpiDatabase::new("http://localhost:11030".to_string());

        let brc20_prog_client = build_brc20_prog_http_client(&config);
        let brc20_reporter = Brc20Reporter::new(&config);

        Brc20Indexer {
            main_db,
            config,
            brc20_prog_client,
            brc20_reporter,
            server_handle: None,
        }
    }

    async fn init(&mut self) -> Result<(), Box<dyn Error>> {
        get_brc20_database().lock().await.init().await?;

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
            if brc20_prog_version != BRC20_PROG_VERSION {
                return Err(format!(
                    "brc20_prog version mismatch, expected {}, got {}",
                    BRC20_PROG_VERSION, brc20_prog_version
                )
                .into());
            }

            let url_clone = self.config.brc20_prog_balance_server_url.clone();
            self.server_handle = Some(tokio::spawn(
                async move { run_balance_server(url_clone).await },
            ));
            // Wait for the server to start
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let brc20_prog_block_height =
                parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?;

            if brc20_prog_block_height == 0 {
                self.brc20_prog_client
                    .brc20_initialise("0".repeat(64).as_str().try_into()?, 0, 0)
                    .await?;
            }
            if brc20_prog_block_height < self.config.first_brc20_prog_phase_one_height {
                self.brc20_prog_client
                    .brc20_mine(
                        (self.config.first_brc20_prog_phase_one_height
                            - brc20_prog_block_height
                            - 1) as u64,
                        0,
                    )
                    .await?;
                self.brc20_prog_client.brc20_commit_to_database().await?;
            }
        }

        Ok(())
    }

    pub async fn clear_caches(&mut self) -> Result<(), Box<dyn Error>> {
        if self.config.brc20_prog_enabled {
            self.brc20_prog_client.brc20_clear_caches().await?;
        }

        let current_block_height = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;

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
            // This doesn't always reorg, but it will reorg if the last block is not the same
            self.reorg_to_last_synced_block_height().await?;

            // Check if a new block is available
            let last_opi_block = self.main_db.get_current_block_height().await?;
            let next_brc20_block = get_brc20_database()
                .lock()
                .await
                .get_next_block_height()
                .await?;
            if next_brc20_block > last_opi_block {
                tracing::info!("Waiting for new blocks...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }

            let is_synced = next_brc20_block == last_opi_block;

            if next_brc20_block % 1000 == 0 {
                tracing::info!(
                    "Clearing brc20 db caches at block height {}",
                    next_brc20_block
                );
                get_brc20_database().lock().await.clear_caches();
            }
            if is_synced || next_brc20_block % 1000 == 0 {
                tracing::info!("Processing block: {}", next_brc20_block);
            }

            let (block_hash, block_time) = self
                .main_db
                .get_block_hash_and_time(next_brc20_block)
                .await?;
            let block_events = self
                .index_block(next_brc20_block, &block_hash, block_time as u64, is_synced)
                .await?;

            if next_brc20_block >= self.config.first_brc20_height
                && get_brc20_database()
                    .lock()
                    .await
                    .should_index_extras(next_brc20_block, last_opi_block)
                    .await?
            {
                // Index extras if synced or close to sync
                get_brc20_database()
                    .lock()
                    .await
                    .index_extra_tables(next_brc20_block)
                    .await?;
            }

            get_brc20_database()
                .lock()
                .await
                .set_block_hash(next_brc20_block, &block_hash)
                .await?;
            let (block_events_hash, cumulative_events_hash) = get_brc20_database()
                .lock()
                .await
                .update_cumulative_hash(next_brc20_block, &block_events)
                .await?;

            // Start reporting after 10 blocks left to full sync
            if next_brc20_block >= last_opi_block - 10 {
                self.brc20_reporter
                    .report(
                        next_brc20_block,
                        block_hash.clone(),
                        block_events_hash.clone(),
                        cumulative_events_hash.clone(),
                    )
                    .await?;
            }
        }
    }

    /// Returns the block events buffer
    pub async fn index_block(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        is_synced: bool,
    ) -> Result<String, Box<dyn Error>> {
        let mut block_events_buffer = String::new();
        let mut brc20_prog_tx_idx: u64 = 0;

        if block_height < self.config.first_brc20_height {
            tracing::info!(
                "Block height {} is less than first_brc20_height {}, skipping",
                block_height,
                self.config.first_brc20_height
            );
            return Ok(block_events_buffer);
        }

        let transfers = self.main_db.get_transfers(block_height).await?;
        if transfers.is_empty() {
            if self.config.brc20_prog_enabled
                && block_height >= self.config.first_brc20_prog_phase_one_height
            {
                self.brc20_prog_client
                    .brc20_finalise_block(block_time, block_hash.try_into()?, brc20_prog_tx_idx)
                    .await?;
                if is_synced || block_height % 100 == 0 {
                    self.brc20_prog_client.brc20_commit_to_database().await?;
                }
            }

            return Ok(block_events_buffer);
        }

        tracing::info!(
            "Transfers found in block {}: {}",
            block_height,
            transfers.len()
        );

        for index in 0..transfers.len() {
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
                        match self
                            .brc20_prog_deploy_transfer(
                                block_height,
                                data,
                                base64_data,
                                block_time,
                                block_hash,
                                brc20_prog_tx_idx,
                                &mut block_events_buffer,
                                transfer,
                            )
                            .await
                        {
                            Ok(_) => {
                                brc20_prog_tx_idx += 1;
                            }
                            _ => {}
                        }
                    } else {
                        self.brc20_prog_deploy_inscribe(
                            block_height,
                            data,
                            base64_data,
                            &mut block_events_buffer,
                            transfer,
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
                        match self
                            .brc20_prog_call_transfer(
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
                                block_time,
                                block_hash,
                                brc20_prog_tx_idx,
                                &mut block_events_buffer,
                                transfer,
                            )
                            .await
                        {
                            Ok(_) => {
                                brc20_prog_tx_idx += 1;
                            }
                            _ => {}
                        }
                    } else {
                        self.brc20_prog_call_inscribe(
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
                            &mut block_events_buffer,
                            transfer,
                        )
                        .await?;
                    }
                } else if operation == OPERATION_BRC20_PROG_TRANSACT
                    || operation == OPERATION_BRC20_PROG_TRANSACT_SHORT
                {
                    if transfer.old_satpoint.is_some() {
                        match self
                            .brc20_prog_transact_transfer(
                                block_height,
                                block_hash,
                                block_time,
                                data,
                                base64_data,
                                brc20_prog_tx_idx,
                                &mut block_events_buffer,
                                transfer,
                            )
                            .await
                        {
                            Ok(txes_executed) => {
                                brc20_prog_tx_idx += txes_executed;
                            }
                            _ => {}
                        }
                    } else {
                        self.brc20_prog_transact_inscribe(
                            block_height,
                            data,
                            base64_data,
                            &mut block_events_buffer,
                            transfer,
                        )
                        .await?;
                    }
                }
                continue;
            }

            if operation == OPERATION_PREDEPLOY && transfer.old_satpoint.is_none() {
                if block_height
                    < self.config.first_brc20_prog_phase_one_height - PREDEPLOY_BLOCK_HEIGHT_DELAY
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

                let predeploy_event = PreDeployInscribeEvent {
                    deployer_pk_script: transfer.new_pkscript.clone(),
                    deployer_wallet: transfer.new_wallet.clone(),
                    hash: hash.to_string(),
                    block_height: block_height,
                };

                block_events_buffer
                    .push_str(&predeploy_event.get_event_str(&transfer.inscription_id, 0));
                block_events_buffer.push_str(EVENT_SEPARATOR);

                get_brc20_database().lock().await.add_event(
                    block_height,
                    &transfer.inscription_id,
                    &transfer.inscription_number,
                    &transfer.old_satpoint,
                    &transfer.new_satpoint,
                    &transfer.txid,
                    &predeploy_event,
                )?;
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
                    match self
                        .brc20_prog_withdraw_transfer(
                            block_height,
                            block_hash,
                            block_time,
                            &deployed_ticker,
                            original_ticker,
                            amount,
                            brc20_prog_tx_idx,
                            &mut block_events_buffer,
                            transfer,
                        )
                        .await
                    {
                        Ok(_) => {
                            brc20_prog_tx_idx += 1;
                        }
                        _ => {}
                    }
                } else {
                    self.brc20_prog_withdraw_inscribe(
                        block_height,
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        &mut block_events_buffer,
                        transfer,
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
                    if block_height < SELF_MINT_ENABLE_HEIGHT {
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

                    let Ok(pkscript_bytes) = hex::decode(transfer.new_pkscript.as_str()) else {
                        tracing::debug!(
                            "Skipping transfer {} as pkscript is not a valid hex string",
                            transfer.inscription_id
                        );
                        continue;
                    };

                    let salted_ticker = [
                        original_ticker.as_bytes(),
                        &salt_bytes,
                        &pkscript_bytes,
                    ].concat();

                    if predeploy_event.hash != sha256::digest(hex::decode(sha256::digest(&salted_ticker))?) {
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

                self.brc20_deploy_inscribe(
                    block_height,
                    &ticker,
                    original_ticker,
                    max_supply,
                    limit_per_mint,
                    decimals,
                    is_self_mint,
                    &mut block_events_buffer,
                    transfer,
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

                self.brc20_mint_inscribe(
                    block_height,
                    &mut deployed_ticker,
                    original_ticker,
                    amount,
                    &mut block_events_buffer,
                    transfer,
                )
                .await?;
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
                    match self
                        .brc20_transfer_transfer(
                            block_height,
                            block_time,
                            block_hash,
                            &mut deployed_ticker,
                            original_ticker,
                            amount,
                            brc20_prog_tx_idx,
                            &mut block_events_buffer,
                            transfer,
                        )
                        .await
                    {
                        Ok(_) => {
                            if transfer.new_pkscript == BRC20_PROG_OP_RETURN_PKSCRIPT {
                                brc20_prog_tx_idx += 1;
                            }
                        }
                        _ => {}
                    }
                } else {
                    self.brc20_transfer_inscribe(
                        block_height,
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        &mut block_events_buffer,
                        transfer,
                    )
                    .await?;
                }
            }
            continue;
        }

        get_brc20_database()
            .lock()
            .await
            .flush_queries_to_db()
            .await?;

        if self.config.brc20_prog_enabled
            && block_height >= self.config.first_brc20_prog_phase_one_height
        {
            self.brc20_prog_client
                .brc20_finalise_block(block_time, block_hash.try_into()?, brc20_prog_tx_idx)
                .await?;
            if is_synced || block_height % 100 == 0 {
                self.brc20_prog_client.brc20_commit_to_database().await?;
            }
        }

        Ok(block_events_buffer
            .trim_end_matches(EVENT_SEPARATOR)
            .to_string())
    }

    pub async fn reorg_to_last_synced_block_height(&mut self) -> Result<(), Box<dyn Error>> {
        tracing::debug!("Reorganizing BRC20 indexer to last synced block height...");
        let mut last_brc20_block_height = get_brc20_database()
            .lock()
            .await
            .get_current_block_height()
            .await?;
        let last_opi_block_height = self.main_db.get_current_block_height().await?;
        let mut last_brc20_prog_block_height = if !self.config.brc20_prog_enabled {
            self.config.first_brc20_prog_phase_one_height
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
                // BRC20 prog is ahead of BRC20, reorg BRC20 to current BRC20 height
                tracing::info!(
                    "BRC20 Prog is ahead of BRC20, reorging BRC20 prog to current BRC20 height: {}",
                    last_brc20_block_height
                );
                self.brc20_prog_client
                    .brc20_reorg(last_brc20_block_height as u64)
                    .await?;

                last_brc20_prog_block_height = last_brc20_block_height;
            } else if last_brc20_prog_block_height < last_brc20_block_height {
                // BRC20 Prog is behind BRC20, reorg BRC20 Prog to current BRC20 height
                tracing::info!(
                    "BRC20 Prog is behind BRC20, reorging BRC20 to current BRC20 prog height: {}",
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
            let opi_block_hash = self.main_db.get_block_hash(current_brc20_height).await?;
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
                if self.config.brc20_prog_enabled {
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

    async fn brc20_prog_deploy_inscribe(
        &mut self,
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgDeployInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Valid);
        Ok(())
    }

    async fn brc20_prog_deploy_transfer(
        &mut self,
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
            .get_transfer_validity(
                &transfer.inscription_id,
                Brc20ProgDeployInscribeEvent::event_id(),
                Brc20ProgDeployTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Transfer is not valid for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = get_brc20_database()
            .lock()
            .await
            .get_event_with_type::<Brc20ProgDeployInscribeEvent>(&transfer.inscription_id)
            .await?
        else {
            tracing::debug!(
                "Inscribe event not found for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("Inscribe event not found")?;
        };
        if transfer.new_pkscript != BRC20_PROG_OP_RETURN_PKSCRIPT {
            tracing::debug!(
                "New pk script is not brc20_prog op return pk script for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("New pk script is not brc20_prog op return pk script")?;
        }
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Used);

        let event = Brc20ProgDeployTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            spent_pk_script: transfer.new_pkscript.to_string(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
            byte_len: transfer.byte_len as i32,
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        self.brc20_prog_client
            .brc20_deploy(
                inscribe_event.source_pk_script,
                data.map(|d| RawBytes::new(d.to_string())),
                base64_data.map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(transfer.inscription_id.to_string()),
                Some(transfer.byte_len as u64),
            )
            .await
            .expect("Failed to deploy smart contract, please check your brc20_prog node");

        Ok(())
    }

    async fn brc20_prog_call_transfer(
        &mut self,
        block_height: i32,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: Option<&str>,
        base64_data: Option<&str>,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
            .get_transfer_validity(
                &transfer.inscription_id,
                Brc20ProgCallInscribeEvent::event_id(),
                Brc20ProgCallTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Transfer is not valid for inscription ID: {}",
                transfer.inscription_id,
            );
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = get_brc20_database()
            .lock()
            .await
            .get_event_with_type::<Brc20ProgCallInscribeEvent>(&transfer.inscription_id)
            .await?
        else {
            tracing::debug!(
                "Inscribe event not found for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("Inscribe event not found")?;
        };
        if transfer.new_pkscript != BRC20_PROG_OP_RETURN_PKSCRIPT {
            tracing::debug!(
                "New pk script is not brc20_prog op return pk script for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("New pk script is not brc20_prog op return pk script")?;
        }
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Used);

        let event = Brc20ProgCallTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            spent_pk_script: transfer.new_pkscript.to_string().into(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
            byte_len: transfer.byte_len as u64,
            contract_address: contract_address.map(|s| s.to_string()),
            contract_inscription_id: contract_inscription_id.map(|s| s.to_string()),
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        let contract_address = if let Some(address) = contract_address {
            Some(address.try_into()?)
        } else {
            None
        };
        self.brc20_prog_client
            .brc20_call(
                inscribe_event.source_pk_script,
                contract_address,
                contract_inscription_id.map(|s| s.to_string()),
                data.map(|d| RawBytes::new(d.to_string())),
                base64_data.map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(transfer.inscription_id.to_string()),
                Some(transfer.byte_len as u64),
            )
            .await
            .expect("Failed to call smart contract, please check your brc20_prog node");

        Ok(())
    }

    async fn brc20_prog_call_inscribe(
        &mut self,
        block_height: i32,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: Option<&str>,
        base64_data: Option<&str>,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgCallInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            contract_address: contract_address.map(|s| s.to_string()).unwrap_or_default(),
            contract_inscription_id: contract_inscription_id
                .map(|s| s.to_string())
                .unwrap_or_default(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Valid);
        Ok(())
    }

    async fn brc20_prog_transact_inscribe(
        &mut self,
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgTransactInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Valid);
        Ok(())
    }

    async fn brc20_prog_transact_transfer(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        data: Option<&str>,
        base64_data: Option<&str>,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<u64, Box<dyn Error>> {
        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
            .get_transfer_validity(
                &transfer.inscription_id,
                Brc20ProgTransactInscribeEvent::event_id(),
                Brc20ProgTransactTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Transfer is not valid for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = get_brc20_database()
            .lock()
            .await
            .get_event_with_type::<Brc20ProgTransactInscribeEvent>(&transfer.inscription_id)
            .await?
        else {
            tracing::debug!(
                "Inscribe event not found for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("Inscribe event not found")?;
        };
        if transfer.new_pkscript != BRC20_PROG_OP_RETURN_PKSCRIPT {
            tracing::debug!(
                "New pk script is not brc20_prog op return pk script for inscription ID: {}",
                transfer.inscription_id
            );
            return Err("New pk script is not brc20_prog op return pk script")?;
        }
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Used);

        let event = Brc20ProgTransactTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            spent_pk_script: transfer.new_pkscript.to_string().into(),
            data: data.map(|d| d.to_string()),
            base64_data: base64_data.map(|b| b.to_string()),
            byte_len: transfer.byte_len as i32,
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        let transact_result = self
            .brc20_prog_client
            .brc20_transact(
                data.map(|d| RawBytes::new(d.to_string())),
                base64_data.map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(transfer.inscription_id.to_string()),
                Some(transfer.byte_len as u64),
            )
            .await
            .expect("Failed to run transact, please check your brc20_prog node");
        Ok(transact_result.len() as u64)
    }

    async fn brc20_prog_withdraw_transfer(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
            .get_transfer_validity(
                &transfer.inscription_id,
                Brc20ProgWithdrawInscribeEvent::event_id(),
                Brc20ProgWithdrawTransferEvent::event_id(),
            )
            .await?
        else {
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = get_brc20_database()
            .lock()
            .await
            .get_event_with_type::<Brc20ProgWithdrawInscribeEvent>(&transfer.inscription_id)
            .await?
        else {
            return Err("Inscribe event not found")?;
        };
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Used);

        let event = Brc20ProgWithdrawTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.clone(),
            spent_pk_script: transfer.new_pkscript.to_string().into(),
            spent_wallet: transfer.new_wallet.to_string().into(),
            ticker: ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
        };
        let event_id = get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer
            .push_str(&event.get_event_str(&transfer.inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        let withdraw_result = self
            .brc20_prog_client
            .brc20_withdraw(
                inscribe_event.source_pk_script.clone(),
                ticker.ticker.clone(),
                amount.into(),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(transfer.inscription_id.to_string()),
            )
            .await
            .expect("Failed to run withdraw, please check your brc20_prog node");

        if !withdraw_result.status.is_zero() {
            let withdraw_to_pkscript = if transfer.sent_as_fee {
                &inscribe_event.source_pk_script
            } else {
                &transfer.new_pkscript
            };

            let withdraw_to_wallet: &str = if transfer.sent_as_fee {
                &inscribe_event.source_wallet
            } else {
                &transfer.new_wallet
            };

            let mut brc20_prog_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&ticker.ticker, &BRC20_PROG_OP_RETURN_PKSCRIPT)
                .await?;

            brc20_prog_balance.overall_balance -= amount;
            brc20_prog_balance.available_balance -= amount;

            // Reduce balance in the BRC20PROG module
            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                &BRC20_PROG_OP_RETURN_PKSCRIPT,
                NO_WALLET,
                &Brc20Balance {
                    overall_balance: brc20_prog_balance.overall_balance,
                    available_balance: brc20_prog_balance.available_balance,
                },
                block_height,
                event_id,
            )?;

            let mut target_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&ticker.ticker, &withdraw_to_pkscript)
                .await?;

            target_balance.overall_balance += amount;
            target_balance.available_balance += amount;

            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                withdraw_to_pkscript,
                withdraw_to_wallet,
                &Brc20Balance {
                    overall_balance: target_balance.overall_balance,
                    available_balance: target_balance.available_balance,
                },
                block_height,
                -event_id, // Negate to create a unique event ID
            )?;
        }
        Ok(())
    }

    async fn brc20_prog_withdraw_inscribe(
        &mut self,
        block_height: i32,
        ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgWithdrawInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string().into(),
            ticker: ticker.ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            amount,
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Valid);
        block_events_buffer
            .push_str(&event.get_event_str(&transfer.inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        Ok(())
    }

    async fn brc20_deploy_inscribe(
        &mut self,
        block_height: i32,
        ticker: &str,
        original_ticker: &str,
        max_supply: u128,
        limit_per_mint: u128,
        decimals: u8,
        is_self_mint: bool,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let event = DeployInscribeEvent {
            deployer_pk_script: transfer.new_pkscript.to_string(),
            deployer_wallet: transfer.new_wallet.to_string(),
            ticker: ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            max_supply,
            limit_per_mint,
            decimals,
            is_self_mint,
        };
        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer.push_str(&event.get_event_str(&transfer.inscription_id, decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        let new_ticker = Ticker {
            ticker: ticker.to_string(),
            remaining_supply: max_supply,
            limit_per_mint,
            decimals,
            is_self_mint,
            deploy_inscription_id: transfer.inscription_id.to_string(),
            original_ticker: original_ticker.to_string(),
            _max_supply: max_supply,
            burned_supply: 0,
            deploy_block_height: block_height,
        };
        get_brc20_database().lock().await.add_ticker(&new_ticker)?;

        Ok(())
    }

    async fn brc20_mint_inscribe(
        &mut self,
        block_height: i32,
        deployed_ticker: &mut Ticker,
        original_ticker: &str,
        mut amount: u128,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        if deployed_ticker.is_self_mint {
            let Some(parent_id) = transfer.parent_id.as_ref() else {
                // Skip if parent id is not present
                tracing::debug!(
                    "Skipping mint {} as parent id is not present for self-mint",
                    transfer.inscription_id
                );
                return Ok(());
            };
            if &deployed_ticker.deploy_inscription_id != parent_id {
                tracing::debug!(
                    "Skipping mint {} as parent id {} does not match deploy inscription id {}",
                    transfer.inscription_id,
                    parent_id,
                    deployed_ticker.deploy_inscription_id
                );
                return Ok(());
            }
        }

        if deployed_ticker.remaining_supply == 0 {
            tracing::debug!(
                "Skipping mint {} as remaining supply is 0 for ticker {}",
                transfer.inscription_id,
                deployed_ticker.ticker
            );
            return Ok(());
        }

        if amount > deployed_ticker.limit_per_mint {
            tracing::debug!(
                "Skipping mint {} as amount {} exceeds limit per mint {} for ticker {}",
                transfer.inscription_id,
                amount,
                deployed_ticker.limit_per_mint,
                deployed_ticker.ticker
            );
            return Ok(());
        }

        if amount > deployed_ticker.remaining_supply {
            // Set amount to remaining supply
            amount = deployed_ticker.remaining_supply;
        }

        let event = MintInscribeEvent {
            minted_pk_script: transfer.new_pkscript.to_string(),
            minted_wallet: transfer.new_wallet.to_string(),
            ticker: deployed_ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
            parent_id: transfer
                .parent_id
                .as_ref()
                .map(|x| x.to_string())
                .unwrap_or_default(),
        };
        let event_id = get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer
            .push_str(&event.get_event_str(&transfer.inscription_id, deployed_ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        deployed_ticker.remaining_supply -= amount;
        get_brc20_database()
            .lock()
            .await
            .update_ticker(deployed_ticker.clone())?;

        let mut balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&deployed_ticker.ticker, &transfer.new_pkscript)
            .await?;

        balance.overall_balance += amount;
        balance.available_balance += amount;

        get_brc20_database().lock().await.update_balance(
            deployed_ticker.ticker.as_str(),
            &transfer.new_pkscript,
            &transfer.new_wallet,
            &balance,
            block_height,
            event_id,
        )?;

        Ok(())
    }

    async fn brc20_transfer_inscribe(
        &mut self,
        block_height: i32,
        ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        let mut balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&ticker.ticker, &transfer.new_pkscript)
            .await?;

        // If available balance is less than amount, return early
        if balance.available_balance < amount {
            tracing::debug!(
                "Skipping transfer {} as available balance {} is less than amount {}",
                transfer.inscription_id,
                balance.available_balance,
                amount
            );
            return Ok(());
        }

        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Valid);

        let event = TransferInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string(),
            ticker: ticker.ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            amount,
        };

        let event_id = get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer
            .push_str(&event.get_event_str(&transfer.inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        balance.available_balance -= amount;

        get_brc20_database().lock().await.update_balance(
            &ticker.ticker,
            &transfer.new_pkscript,
            &transfer.new_wallet,
            &balance,
            block_height,
            event_id,
        )?;

        Ok(())
    }

    async fn brc20_transfer_transfer(
        &mut self,
        block_height: i32,
        block_time: u64,
        block_hash: &str,
        ticker: &mut Ticker,
        original_ticker: &str,
        amount: u128,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
        transfer: &BRC20Tx,
    ) -> Result<(), Box<dyn Error>> {
        tracing::debug!(
            "Processing transfer for inscription ID: {}, new pk script: {}, new wallet: {:?}, ticker: {}, original ticker: {}, amount: {}, sent as fee: {}, tx_id: {}",
            transfer.inscription_id,
            transfer.new_pkscript,
            transfer.new_wallet,
            ticker.ticker,
            original_ticker,
            amount,
            transfer.sent_as_fee,
            transfer.tx_id
        );
        let Some(inscribe_event) = get_brc20_database()
            .lock()
            .await
            .get_event_with_type::<TransferInscribeEvent>(&transfer.inscription_id)
            .await?
        else {
            tracing::debug!(
                "Skipping transfer {} as inscribe event is not found",
                transfer.inscription_id
            );
            return Err("Inscribe event not found")?;
        };

        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
            .get_transfer_validity(
                &transfer.inscription_id,
                TransferInscribeEvent::event_id(),
                TransferTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Skipping transfer {} as transfer is not valid",
                transfer.inscription_id
            );
            return Err("Transfer is not valid")?;
        };

        let event = TransferTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.to_string(),
            spent_pk_script: if transfer.sent_as_fee {
                None
            } else {
                Some(transfer.new_pkscript.to_string())
            },
            spent_wallet: if transfer.sent_as_fee {
                None
            } else {
                Some(transfer.new_wallet.clone())
            },
            ticker: ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
            tx_id: transfer.tx_id.clone(),
        };

        let event_id = get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &event,
        )?;
        block_events_buffer
            .push_str(&event.get_event_str(&transfer.inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        // TODO: Reduce overall balance of the source wallet

        // If sent as fee, return to the source wallet
        // If sent to BRC20_PROG_OP_RETURN_PKSCRIPT, send to brc20_prog
        // If sent to OP_RETURN, update burned supply
        // If sent to a wallet, update the wallet balance
        let mut source_balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
            .await?;
        if transfer.sent_as_fee {
            source_balance.available_balance += amount;

            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                &inscribe_event.source_pk_script,
                &inscribe_event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;
        } else if transfer.new_pkscript == BRC20_PROG_OP_RETURN_PKSCRIPT {
            if (block_height < self.config.first_brc20_prog_all_tickers_height
                && original_ticker.as_bytes().len() < 6)
                || block_height < self.config.first_brc20_prog_phase_one_height
                || !self.config.brc20_prog_enabled
            {
                // Burn tokens if BRC20 Prog is not enabled for this ticker yet
                source_balance.overall_balance -= amount;
                get_brc20_database().lock().await.update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

                ticker.burned_supply += amount;
                get_brc20_database()
                    .lock()
                    .await
                    .update_ticker(ticker.clone())?;

                Err("Burning tokens, BRC20 Prog is not enabled yet")?;
            } else {
                source_balance.overall_balance -= amount;

                get_brc20_database().lock().await.update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

                let mut brc20_prog_balance = get_brc20_database()
                    .lock()
                    .await
                    .get_balance(&ticker.ticker, BRC20_PROG_OP_RETURN_PKSCRIPT)
                    .await?;

                brc20_prog_balance.available_balance += amount;
                brc20_prog_balance.overall_balance += amount;

                get_brc20_database().lock().await.update_balance(
                    &ticker.ticker,
                    BRC20_PROG_OP_RETURN_PKSCRIPT,
                    NO_WALLET,
                    &brc20_prog_balance,
                    block_height,
                    -event_id, // Negate to create a unique event ID
                )?;
                self.brc20_prog_client
                    .brc20_deposit(
                        inscribe_event.source_pk_script,
                        ticker.ticker.clone(),
                        amount.into(),
                        block_time,
                        block_hash.try_into()?,
                        brc20_prog_tx_idx,
                        Some(transfer.inscription_id.to_string()),
                    )
                    .await?;
            }
        } else if transfer.new_pkscript == OP_RETURN {
            let mut source_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
                .await?;

            source_balance.overall_balance -= amount;

            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                &inscribe_event.source_pk_script,
                &inscribe_event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;

            // Update burned supply
            ticker.burned_supply += amount;
            get_brc20_database()
                .lock()
                .await
                .update_ticker(ticker.clone())?;
        } else {
            let mut source_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
                .await?;

            source_balance.overall_balance -= amount;

            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                &inscribe_event.source_pk_script,
                &inscribe_event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;

            let mut target_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&ticker.ticker, &transfer.new_pkscript)
                .await?;

            target_balance.available_balance += amount;
            target_balance.overall_balance += amount;

            get_brc20_database().lock().await.update_balance(
                &ticker.ticker,
                &transfer.new_pkscript,
                &transfer.new_wallet,
                &target_balance,
                block_height,
                -event_id, // Negate to create a unique event ID
            )?;
        }
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&transfer.inscription_id, TransferValidity::Used);

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
