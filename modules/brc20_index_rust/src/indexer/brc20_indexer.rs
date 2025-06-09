use std::error::Error;

use brc20_prog::{Brc20ProgApiClient, types::InscriptionBytes};
use jsonrpsee::http_client::HttpClient;
use tokio::task::JoinHandle;

use crate::{
    config::{
        AMOUNT_KEY, BRC20_MODULE_BRC20PROG, BRC20_PROG_OP_RETURN_PKSCRIPT, BRC20_PROG_VERSION,
        Brc20IndexerConfig, CONTRACT_ADDRESS_KEY, DATA_KEY, DB_VERSION, DECIMALS_KEY,
        EVENT_SEPARATOR, INSCRIPTION_ID_KEY, LIMIT_PER_MINT_KEY, MAX_AMOUNT, MAX_SUPPLY_KEY,
        MODULE_KEY, NO_WALLET, OP_RETURN, OPERATION_BRC20_PROG_CALL,
        OPERATION_BRC20_PROG_CALL_SHORT, OPERATION_BRC20_PROG_DEPLOY,
        OPERATION_BRC20_PROG_DEPLOY_SHORT, OPERATION_DEPLOY, OPERATION_KEY, OPERATION_MINT,
        OPERATION_TRANSFER, OPERATION_WITHDRAW, PROTOCOL_BRC20, PROTOCOL_BRC20_MODULE,
        PROTOCOL_BRC20_PROG, PROTOCOL_KEY, SELF_MINT_ENABLE_HEIGHT, SELF_MINT_KEY, TICKER_KEY,
    },
    database::{Brc20Balance, Brc20Database, OpiDatabase, TransferValidity},
    default,
    indexer::{
        brc20_prog_client::build_brc20_prog_http_client,
        brc20_reporter::Brc20Reporter,
        utils::{ALLOW_ZERO, DISALLOW_ZERO, get_amount_value, get_decimals_value},
    },
    no_default,
    types::{
        Ticker,
        events::{
            Brc20ProgCallInscribeEvent, Brc20ProgCallTransferEvent, Brc20ProgDeployInscribeEvent,
            Brc20ProgDeployTransferEvent, Brc20ProgWithdrawInscribeEvent,
            Brc20ProgWithdrawTransferEvent, DeployInscribeEvent, Event, MintInscribeEvent,
            TransferInscribeEvent, TransferTransferEvent,
        },
    },
};

use super::brc20_prog_balance_server::run_balance_server;

pub struct Brc20Indexer {
    pub brc20_db: Brc20Database,
    pub main_db: OpiDatabase,
    pub config: Brc20IndexerConfig,
    pub brc20_prog_client: HttpClient,
    pub brc20_reporter: Brc20Reporter,
    pub server_handle: Option<JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>>,
}

impl Brc20Indexer {
    pub fn new(config: Brc20IndexerConfig) -> Self {
        let brc20_db = Brc20Database::new(&config);
        let main_db = OpiDatabase::new(
            "http://localhost:11030".to_string()
        );

        let brc20_prog_client = build_brc20_prog_http_client(&config);
        let brc20_reporter = Brc20Reporter::new(&config);

        Brc20Indexer {
            brc20_db,
            main_db,
            config,
            brc20_prog_client,
            brc20_reporter,
            server_handle: None,
        }
    }

    async fn init(&mut self) -> Result<(), Box<dyn Error>> {
        let db_version = self.brc20_db.get_db_version().await?;
        if db_version != DB_VERSION {
            return Err(format!(
                "db_version mismatch, expected {}, got {}, please run brc20_indexer with --reset",
                DB_VERSION, db_version
            )
            .into());
        }
        self.brc20_db.init().await?;

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

            self.server_handle = Some(tokio::spawn(async {
                run_balance_server(Default::default()).await
            }));
            // Wait for the server to start
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let brc20_prog_block_height =
                parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?;

            if brc20_prog_block_height == 0 {
                self.brc20_prog_client
                    .brc20_initialise("0".repeat(64).as_str().try_into()?, 0, 0)
                    .await?;
            }
            if brc20_prog_block_height < self.config.first_brc20_prog_height {
                self.brc20_prog_client
                    .brc20_mine(
                        (self.config.first_brc20_prog_height - brc20_prog_block_height - 1) as u64,
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

        let current_block_height = self.brc20_db.get_current_block_height().await?;

        if self.brc20_db.check_residue(current_block_height).await? {
            tracing::debug!(
                "BRC20 indexer residue found at block height {}, reorging to last synced block height",
                current_block_height
            );
            self.brc20_db.reorg(current_block_height).await?;
            if self.config.brc20_prog_enabled
                && current_block_height >= self.config.first_brc20_prog_height
            {
                self.brc20_prog_client
                    .brc20_reorg(current_block_height as u64)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn reset(&mut self) -> Result<(), Box<dyn Error>> {
        tracing::info!("Resetting BRC20 indexer database...");
        self.brc20_db.reset().await?;
        self.brc20_db.init().await?;
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
            let next_brc20_block = self.brc20_db.get_next_block_height().await?;
            if next_brc20_block > last_opi_block {
                tracing::info!("Waiting for new blocks...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }

            let is_synced = next_brc20_block == last_opi_block;

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

            self.brc20_db
                .set_block_hash(next_brc20_block, &block_hash)
                .await?;
            let (block_events_hash, cumulative_events_hash) = self
                .brc20_db
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
            if self.config.brc20_prog_enabled && block_height >= self.config.first_brc20_prog_height
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

            let Ok(content_type) = hex::decode(transfer.content_type.as_str()) else {
                tracing::debug!(
                    "Skipping transfer {} as content type is not valid hex",
                    transfer.inscription_id
                );
                continue;
            };

            let Ok(decoded_content_type) = String::from_utf8(content_type) else {
                tracing::debug!(
                    "Skipping transfer {} as content type is not valid UTF-8",
                    transfer.inscription_id
                );
                continue;
            };

            if !decoded_content_type.starts_with("application/json")
                && !decoded_content_type.starts_with("text/plain")
            {
                tracing::debug!(
                    "Skipping transfer {} as content type is not application/json or text/plain",
                    transfer.inscription_id
                );
                continue;
            }

            let Some(content) = &transfer.content else {
                tracing::debug!(
                    "Skipping transfer {} as content is not present",
                    transfer.inscription_id
                );
                continue;
            };

            let Some(protocol) = content.get(PROTOCOL_KEY).and_then(|p| p.as_str()) else {
                tracing::debug!(
                    "Skipping transfer {} as protocol is not present",
                    transfer.inscription_id
                );
                continue;
            };

            if protocol != PROTOCOL_BRC20 && protocol != PROTOCOL_BRC20_PROG && protocol != PROTOCOL_BRC20_MODULE {
                tracing::debug!(
                    "Skipping transfer {} as protocol is not BRC20 or BRC20 Prog",
                    transfer.inscription_id
                );
                continue;
            }

            let Some(operation) = content.get(OPERATION_KEY).and_then(|op| op.as_str()) else {
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

                if block_height < self.config.first_brc20_prog_height {
                    tracing::debug!(
                        "Skipping transfer {} as block height {} is less than first BRC20 Prog height {}",
                        transfer.inscription_id,
                        block_height,
                        self.config.first_brc20_prog_height
                    );
                    continue;
                }

                let Some(data) = content.get(DATA_KEY).and_then(|d| d.as_str()) else {
                    tracing::debug!(
                        "Skipping transfer {} as data is not present",
                        transfer.inscription_id
                    );
                    continue;
                };

                if operation == OPERATION_BRC20_PROG_DEPLOY
                    || operation == OPERATION_BRC20_PROG_DEPLOY_SHORT
                {
                    if transfer.old_satpoint.is_some() {
                        match self
                            .brc20_prog_deploy_transfer(
                                block_height,
                                &transfer.inscription_id,
                                &transfer.new_pkscript,
                                data,
                                transfer.byte_length,
                                block_time,
                                block_hash,
                                brc20_prog_tx_idx,
                                &mut block_events_buffer,
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
                            &transfer.inscription_id,
                            &transfer.new_pkscript,
                            data,
                            &mut block_events_buffer,
                        )?;
                    }
                } else if operation == OPERATION_BRC20_PROG_CALL
                    || operation == OPERATION_BRC20_PROG_CALL_SHORT
                {
                    if content.get(CONTRACT_ADDRESS_KEY).is_none()
                        && content.get(INSCRIPTION_ID_KEY).is_none()
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
                                &transfer.inscription_id,
                                &transfer.new_pkscript,
                                content.get(CONTRACT_ADDRESS_KEY).and_then(|c| c.as_str()),
                                content.get(INSCRIPTION_ID_KEY).and_then(|i| i.as_str()),
                                data,
                                transfer.byte_length,
                                block_time,
                                block_hash,
                                brc20_prog_tx_idx,
                                &mut block_events_buffer,
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
                            &transfer.inscription_id,
                            &transfer.new_pkscript,
                            content.get(CONTRACT_ADDRESS_KEY).and_then(|c| c.as_str()),
                            content.get(INSCRIPTION_ID_KEY).and_then(|i| i.as_str()),
                            data,
                            &mut block_events_buffer,
                        )?;
                    }
                }
                continue;
            }

            let Some(original_ticker) = content.get(TICKER_KEY).and_then(|ot| ot.as_str()) else {
                tracing::debug!(
                    "Skipping transfer {} as ticker is not present",
                    transfer.inscription_id
                );
                continue;
            };

            let ticker = original_ticker.to_lowercase();

            if ticker.as_bytes().len() != 4 && ticker.as_bytes().len() != 5 {
                tracing::debug!(
                    "Skipping transfer {} as ticker length is not 4 or 5 bytes",
                    transfer.inscription_id
                );
                continue;
            }

            if protocol == PROTOCOL_BRC20_MODULE {
                let Ok(Some(deployed_ticker)) = self.brc20_db.get_ticker(&ticker) else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Some(module) = content.get(MODULE_KEY).and_then(|m| m.as_str()) else {
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
                    content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
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
                            &transfer.inscription_id,
                            &transfer.new_pkscript,
                            transfer.new_wallet.as_ref(),
                            amount,
                            transfer.sent_as_fee,
                            brc20_prog_tx_idx,
                            &mut block_events_buffer,
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
                        &transfer.inscription_id,
                        &transfer.new_pkscript,
                        transfer.new_wallet.as_ref().map_or(NO_WALLET, |w| w),
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        &mut block_events_buffer,
                    )?;
                }
                continue;
            }

            if operation == OPERATION_DEPLOY && transfer.old_satpoint.is_none() {
                if let Ok(Some(_)) = self.brc20_db.get_ticker(&ticker) {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is already deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                }

                let Ok(decimals) =
                    get_decimals_value(content.get(DECIMALS_KEY).and_then(|d| d.as_str()))
                else {
                    tracing::debug!(
                        "Skipping transfer {} as decimals are not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                let Ok(mut max_supply) = get_amount_value(
                    content.get(MAX_SUPPLY_KEY).and_then(|m| m.as_str()),
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

                let Ok(mut limit_per_mint) = get_amount_value(
                    content.get(LIMIT_PER_MINT_KEY).and_then(|l| l.as_str()),
                    decimals,
                    default!(max_supply),
                    ALLOW_ZERO,
                ) else {
                    tracing::debug!(
                        "Skipping transfer {} as limit per mint is not present or invalid",
                        transfer.inscription_id
                    );
                    continue;
                };

                let mut is_self_mint = false;
                if original_ticker.as_bytes().len() == 5 {
                    if block_height < SELF_MINT_ENABLE_HEIGHT {
                        tracing::debug!(
                            "Skipping transfer {} as self mint is not enabled yet",
                            transfer.inscription_id
                        );
                        continue;
                    }
                    if let Some(self_mint) = content.get(SELF_MINT_KEY).and_then(|s| s.as_str()) {
                        if self_mint != "true" {
                            tracing::debug!(
                                "Skipping transfer {} as self mint is not enabled",
                                transfer.inscription_id
                            );
                            continue;
                        }
                    }
                    is_self_mint = true;
                    if max_supply == 0 {
                        max_supply = MAX_AMOUNT;
                        if limit_per_mint == 0 {
                            limit_per_mint = MAX_AMOUNT;
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
                    &transfer.inscription_id,
                    &transfer.new_pkscript,
                    transfer.new_wallet.as_ref().map_or(NO_WALLET, |w| w),
                    &ticker,
                    original_ticker,
                    max_supply,
                    limit_per_mint,
                    decimals,
                    is_self_mint,
                    &mut block_events_buffer,
                )?;

                continue;
            }

            if operation == OPERATION_MINT && transfer.old_satpoint.is_none() {
                let Ok(Some(mut deployed_ticker)) = self.brc20_db.get_ticker(&ticker) else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Ok(amount) = get_amount_value(
                    content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
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
                    &transfer.inscription_id,
                    &transfer.new_pkscript,
                    transfer.new_wallet.as_ref().map_or(NO_WALLET, |w| w),
                    &mut deployed_ticker,
                    original_ticker,
                    amount,
                    transfer.parent_inscription_id.clone(),
                    &mut block_events_buffer,
                )
                .await?;
                continue;
            }

            if operation == OPERATION_TRANSFER {
                let Ok(Some(mut deployed_ticker)) = self.brc20_db.get_ticker(&ticker) else {
                    tracing::debug!(
                        "Skipping transfer {} as ticker {} is not deployed",
                        transfer.inscription_id,
                        ticker
                    );
                    continue;
                };

                let Ok(amount) = get_amount_value(
                    content.get(AMOUNT_KEY).and_then(|a| a.as_str()),
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
                            &transfer.inscription_id,
                            &transfer.new_pkscript,
                            transfer.new_wallet.as_ref(),
                            &mut deployed_ticker,
                            original_ticker,
                            amount,
                            transfer.sent_as_fee,
                            transfer.tx_id.clone(),
                            brc20_prog_tx_idx,
                            &mut block_events_buffer,
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
                        &transfer.inscription_id,
                        &transfer.new_pkscript,
                        transfer.new_wallet.as_ref().map_or(NO_WALLET, |w| w),
                        &deployed_ticker,
                        original_ticker,
                        amount,
                        &mut block_events_buffer,
                    )
                    .await?;
                }
            }
            continue;
        }
        
        self.brc20_db.flush_queries_to_db().await?;

        if self.config.brc20_prog_enabled && block_height >= self.config.first_brc20_prog_height {
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
        let mut last_brc20_block_height = self.brc20_db.get_current_block_height().await?;
        let last_opi_block_height = self.main_db.get_current_block_height().await?;
        let mut last_brc20_prog_block_height = if !self.config.brc20_prog_enabled {
            self.config.first_brc20_prog_height
        } else {
            parse_hex_number(&self.brc20_prog_client.eth_block_number().await?)?
        };

        // BRC20 indexer has not indexed any blocks yet, no need to reorg BRC20
        if last_brc20_block_height < self.config.first_brc20_height {
            if self.config.brc20_prog_enabled
                && last_brc20_prog_block_height > self.config.first_brc20_prog_height
            {
                self.brc20_prog_client
                    .brc20_reorg(self.config.first_brc20_prog_height as u64)
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
            && last_brc20_prog_block_height >= self.config.first_brc20_prog_height
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
                self.brc20_db
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

            let brc20_block_hash = self.brc20_db.get_block_hash(current_brc20_height).await?;
            let opi_block_hash = self.main_db.get_block_hash(current_brc20_height).await?;
            let brc20_prog_block_hash = if !self.config.brc20_prog_enabled
                || current_brc20_height < self.config.first_brc20_prog_height
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
                self.brc20_db.reorg(current_brc20_height).await?;
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
                self.brc20_db.reorg(self.config.first_brc20_height).await?;
                if self.config.brc20_prog_enabled {
                    self.brc20_prog_client
                        .brc20_reorg(self.config.first_brc20_prog_height as u64)
                        .await?;
                }
                break;
            }
            current_brc20_height -= 1;
            current_brc20_prog_height -= 1;
        }

        Ok(())
    }

    pub fn brc20_prog_deploy_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        source_pk_script: &str,
        data: &str,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgDeployInscribeEvent {
            source_pk_script: source_pk_script.to_string(),
            data: data.to_string(),
        };
        self.brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        self.brc20_db
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        Ok(())
    }

    pub async fn brc20_prog_deploy_transfer(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        data: &str,
        byte_length: i32,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = self
            .brc20_db
            .get_transfer_validity(
                &inscription_id,
                Brc20ProgDeployInscribeEvent::event_id(),
                Brc20ProgDeployTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Transfer is not valid for inscription ID: {}",
                inscription_id
            );
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = self
            .brc20_db
            .get_event_with_type::<Brc20ProgDeployInscribeEvent>(&inscription_id)
            .await?
        else {
            tracing::debug!(
                "Inscribe event not found for inscription ID: {}",
                inscription_id
            );
            return Err("Inscribe event not found")?;
        };
        if new_pkscript != BRC20_PROG_OP_RETURN_PKSCRIPT {
            tracing::debug!(
                "New pk script is not brc20_prog op return pk script for inscription ID: {}",
                inscription_id
            );
            return Err("New pk script is not brc20_prog op return pk script")?;
        }
        self.brc20_db
            .set_transfer_validity(&inscription_id, TransferValidity::Used);

        let event = Brc20ProgDeployTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            spent_pk_script: new_pkscript.to_string(),
            data: data.to_string(),
            byte_len: byte_length,
        };
        self.brc20_db
            .add_event(block_height, &inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(&inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        self.brc20_prog_client
            .brc20_deploy(
                inscribe_event.source_pk_script,
                InscriptionBytes::new(data.to_string()),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
                Some(byte_length as u64),
            )
            .await
            .expect("Failed to deploy smart contract, please check your brc20_prog node");

        Ok(())
    }

    async fn brc20_prog_call_transfer(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: &str,
        byte_length: i32,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = self
            .brc20_db
            .get_transfer_validity(
                &inscription_id,
                Brc20ProgCallInscribeEvent::event_id(),
                Brc20ProgCallTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Transfer is not valid for inscription ID: {}",
                inscription_id
            );
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = self
            .brc20_db
            .get_event_with_type::<Brc20ProgCallInscribeEvent>(&inscription_id)
            .await?
        else {
            tracing::debug!(
                "Inscribe event not found for inscription ID: {}",
                inscription_id
            );
            return Err("Inscribe event not found")?;
        };
        if new_pkscript != BRC20_PROG_OP_RETURN_PKSCRIPT {
            tracing::debug!(
                "New pk script is not brc20_prog op return pk script for inscription ID: {}",
                inscription_id
            );
            return Err("New pk script is not brc20_prog op return pk script")?;
        }
        self.brc20_db
            .set_transfer_validity(&inscription_id, TransferValidity::Used);

        let event = Brc20ProgCallTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            spent_pk_script: new_pkscript.to_string().into(),
            data: data.to_string(),
            byte_len: byte_length as u64,
            contract_address: contract_address.map(|s| s.to_string()),
            contract_inscription_id: contract_inscription_id.map(|s| s.to_string()),
        };
        self.brc20_db
            .add_event(block_height, &inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(&inscription_id, 0));
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
                InscriptionBytes::new(data.to_string()),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
                Some(byte_length as u64),
            )
            .await
            .expect("Failed to call smart contract, please check your brc20_prog node");

        Ok(())
    }

    fn brc20_prog_call_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: &str,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgCallInscribeEvent {
            source_pk_script: new_pkscript.to_string(),
            contract_address: contract_address.map(|s| s.to_string()).unwrap_or_default(),
            contract_inscription_id: contract_inscription_id
                .map(|s| s.to_string())
                .unwrap_or_default(),
            data: data.to_string(),
        };
        self.brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(inscription_id, 0));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        self.brc20_db
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        Ok(())
    }

    async fn brc20_prog_withdraw_transfer(
        &mut self,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        ticker: &Ticker,
        original_ticker: &str,
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: Option<&String>,
        amount: u128,
        sent_as_fee: bool,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let TransferValidity::Valid = self
            .brc20_db
            .get_transfer_validity(
                &inscription_id,
                Brc20ProgWithdrawInscribeEvent::event_id(),
                Brc20ProgWithdrawTransferEvent::event_id(),
            )
            .await?
        else {
            return Err("Transfer is not valid")?;
        };
        let Some(inscribe_event) = self
            .brc20_db
            .get_event_with_type::<Brc20ProgWithdrawInscribeEvent>(&inscription_id)
            .await?
        else {
            return Err("Inscribe event not found")?;
        };
        self.brc20_db
            .set_transfer_validity(&inscription_id, TransferValidity::Used);

        let event = Brc20ProgWithdrawTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.clone(),
            spent_pk_script: new_pkscript.to_string().into(),
            spent_wallet: new_wallet.map(|s| s.to_string()),
            ticker: ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
        };
        let event_id = self
            .brc20_db
            .add_event(block_height, &inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(&inscription_id, ticker.decimals));
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
                Some(inscription_id.to_string()),
            )
            .await
            .expect("Failed to run withdraw, please check your brc20_prog node");

        if !withdraw_result.status.is_zero() {
            let withdraw_to_pkscript = if sent_as_fee {
                &inscribe_event.source_pk_script
            } else {
                new_pkscript
            };

            let withdraw_to_wallet: &str = if sent_as_fee {
                &inscribe_event.source_wallet
            } else {
                new_wallet.map_or(NO_WALLET, |w| w)
            };

            let mut brc20_prog_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, &BRC20_PROG_OP_RETURN_PKSCRIPT)
                .await?;

            brc20_prog_balance.overall_balance -= amount;
            brc20_prog_balance.available_balance -= amount;

            // Reduce balance in the BRC20PROG module
            self.brc20_db
                .update_balance(
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

            let mut target_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, &withdraw_to_pkscript)
                .await?;

            target_balance.overall_balance += amount;
            target_balance.available_balance += amount;

            self.brc20_db
                .update_balance(
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

    fn brc20_prog_withdraw_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: &str,
        ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let event = Brc20ProgWithdrawInscribeEvent {
            source_pk_script: new_pkscript.to_string(),
            source_wallet: new_wallet.to_string().into(),
            ticker: ticker.ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            amount,
        };
        self.brc20_db
            .add_event(block_height, inscription_id, &event)?;
        self.brc20_db
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        block_events_buffer.push_str(&event.get_event_str(inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);
        Ok(())
    }

    fn brc20_deploy_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: &str,
        ticker: &str,
        original_ticker: &str,
        max_supply: u128,
        limit_per_mint: u128,
        decimals: u8,
        is_self_mint: bool,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let event = DeployInscribeEvent {
            deployer_pk_script: new_pkscript.to_string(),
            deployer_wallet: new_wallet.to_string(),
            ticker: ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            max_supply,
            limit_per_mint,
            decimals,
            is_self_mint,
        };
        self.brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(inscription_id, decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        let new_ticker = Ticker {
            ticker: ticker.to_string(),
            remaining_supply: max_supply,
            limit_per_mint,
            decimals,
            is_self_mint,
            deploy_inscription_id: inscription_id.to_string(),
            original_ticker: original_ticker.to_string(),
            _max_supply: max_supply,
            burned_supply: 0,
            deploy_block_height: block_height,
        };
        self.brc20_db.add_ticker(&new_ticker)?;

        Ok(())
    }

    async fn brc20_mint_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: &str,
        deployed_ticker: &mut Ticker,
        original_ticker: &str,
        mut amount: u128,
        parent_id: Option<String>,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        if deployed_ticker.is_self_mint {
            let Some(parent_id) = parent_id.as_ref() else {
                // Skip if parent id is not present
                tracing::debug!(
                    "Skipping mint {} as parent id is not present for self-mint",
                    inscription_id
                );
                return Ok(());
            };
            if &deployed_ticker.deploy_inscription_id != parent_id {
                tracing::debug!(
                    "Skipping mint {} as parent id {} does not match deploy inscription id {}",
                    inscription_id,
                    parent_id,
                    deployed_ticker.deploy_inscription_id
                );
                return Ok(());
            }
        }

        if deployed_ticker.remaining_supply == 0 {
            tracing::debug!(
                "Skipping mint {} as remaining supply is 0 for ticker {}",
                inscription_id,
                deployed_ticker.ticker
            );
            return Ok(());
        }

        if amount > deployed_ticker.limit_per_mint {
            tracing::debug!(
                "Skipping mint {} as amount {} exceeds limit per mint {} for ticker {}",
                inscription_id,
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
            minted_pk_script: new_pkscript.to_string(),
            minted_wallet: new_wallet.to_string(),
            ticker: deployed_ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
            parent_id: parent_id.unwrap_or_default(),
        };
        let event_id = self
            .brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer
            .push_str(&event.get_event_str(inscription_id, deployed_ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        deployed_ticker.remaining_supply -= amount;
        self.brc20_db.update_ticker(deployed_ticker.clone())?;

        let mut balance = self
            .brc20_db
            .get_balance(&deployed_ticker.ticker, new_pkscript)
            .await?;

        balance.overall_balance += amount;
        balance.available_balance += amount;

        self.brc20_db
            .update_balance(
                deployed_ticker.ticker.as_str(),
                new_pkscript,
                new_wallet,
                &balance,
                block_height,
                event_id,
            )?;

        Ok(())
    }

    async fn brc20_transfer_inscribe(
        &mut self,
        block_height: i32,
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: &str,
        ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        let mut balance = self
            .brc20_db
            .get_balance(&ticker.ticker, new_pkscript)
            .await?;

        // If available balance is less than amount, return early
        if balance.available_balance < amount {
            tracing::debug!(
                "Skipping transfer {} as available balance {} is less than amount {}",
                inscription_id,
                balance.available_balance,
                amount
            );
            return Ok(());
        }

        self.brc20_db
            .set_transfer_validity(inscription_id, TransferValidity::Valid);

        let event = TransferInscribeEvent {
            source_pk_script: new_pkscript.to_string(),
            source_wallet: new_wallet.to_string(),
            ticker: ticker.ticker.to_string(),
            original_ticker: original_ticker.to_string(),
            amount,
        };

        let event_id = self
            .brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        balance.available_balance -= amount;

        self.brc20_db
            .update_balance(
                &ticker.ticker,
                new_pkscript,
                new_wallet,
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
        inscription_id: &str,
        new_pkscript: &str,
        new_wallet: Option<&String>,
        ticker: &mut Ticker,
        original_ticker: &str,
        amount: u128,
        sent_as_fee: bool,
        tx_id: String,
        brc20_prog_tx_idx: u64,
        block_events_buffer: &mut String,
    ) -> Result<(), Box<dyn Error>> {
        tracing::debug!(
            "Processing transfer for inscription ID: {}, new pk script: {}, new wallet: {:?}, ticker: {}, original ticker: {}, amount: {}, sent as fee: {}, tx_id: {}",
            inscription_id,
            new_pkscript,
            new_wallet,
            ticker.ticker,
            original_ticker,
            amount,
            sent_as_fee,
            tx_id
        );
        let Some(inscribe_event) = self
            .brc20_db
            .get_event_with_type::<TransferInscribeEvent>(inscription_id)
            .await?
        else {
            tracing::debug!(
                "Skipping transfer {} as inscribe event is not found",
                inscription_id
            );
            return Err("Inscribe event not found")?;
        };

        let TransferValidity::Valid = self
            .brc20_db
            .get_transfer_validity(
                inscription_id,
                TransferInscribeEvent::event_id(),
                TransferTransferEvent::event_id(),
            )
            .await?
        else {
            tracing::debug!(
                "Skipping transfer {} as transfer is not valid",
                inscription_id
            );
            return Err("Transfer is not valid")?;
        };

        let event = TransferTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.to_string(),
            spent_pk_script: if sent_as_fee {
                None
            } else {
                Some(new_pkscript.to_string())
            },
            spent_wallet: if sent_as_fee {
                None
            } else {
                Some(new_wallet.map(|x| x.to_string()).unwrap_or_default())
            },
            ticker: ticker.ticker.clone(),
            original_ticker: original_ticker.to_string(),
            amount,
            tx_id,
        };

        let event_id = self
            .brc20_db
            .add_event(block_height, inscription_id, &event)?;
        block_events_buffer.push_str(&event.get_event_str(inscription_id, ticker.decimals));
        block_events_buffer.push_str(EVENT_SEPARATOR);

        // TODO: Reduce overall balance of the source wallet

        // If sent as fee, return to the source wallet
        // If sent to BRC20_PROG_OP_RETURN_PKSCRIPT, send to brc20_prog
        // If sent to OP_RETURN, update burned supply
        // If sent to a wallet, update the wallet balance
        let mut source_balance = self
            .brc20_db
            .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
            .await?;
        if sent_as_fee {
            source_balance.available_balance += amount;

            self.brc20_db
                .update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;
        } else if new_pkscript == BRC20_PROG_OP_RETURN_PKSCRIPT {
            source_balance.overall_balance -= amount;

            self.brc20_db
                .update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

            let mut brc20_prog_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, BRC20_PROG_OP_RETURN_PKSCRIPT)
                .await?;

            brc20_prog_balance.available_balance += amount;
            brc20_prog_balance.overall_balance += amount;

            self.brc20_db
                .update_balance(
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
                    Some(inscription_id.to_string()),
                )
                .await?;
        } else if new_pkscript == OP_RETURN {
            let mut source_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
                .await?;

            source_balance.overall_balance -= amount;

            self.brc20_db
                .update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

            // Update burned supply
            ticker.burned_supply += amount;
            self.brc20_db.update_ticker(ticker.clone())?;
        } else {
            let mut source_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, &inscribe_event.source_pk_script)
                .await?;

            source_balance.overall_balance -= amount;

            self.brc20_db
                .update_balance(
                    &ticker.ticker,
                    &inscribe_event.source_pk_script,
                    &inscribe_event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

            let mut target_balance = self
                .brc20_db
                .get_balance(&ticker.ticker, new_pkscript)
                .await?;

            target_balance.available_balance += amount;
            target_balance.overall_balance += amount;

            self.brc20_db
                .update_balance(
                    &ticker.ticker,
                    new_pkscript,
                    new_wallet.map_or(NO_WALLET, |w| w),
                    &target_balance,
                    block_height,
                    -event_id, // Negate to create a unique event ID
                )?;
        }
        self.brc20_db
            .set_transfer_validity(inscription_id, TransferValidity::Used);

        Ok(())
    }
}

fn parse_hex_number(eth_number: &str) -> Result<i32, Box<dyn Error>> {
    i32::from_str_radix(eth_number.trim_start_matches("0x"), 16).map_err(|e| e.into())
}
