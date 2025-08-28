use std::error::Error;

use brc20_prog::{
    Brc20ProgApiClient,
    types::{Base64Bytes, RawBytes},
};
use jsonrpsee::http_client::HttpClient;

use crate::{
    config::{BRC20_PROG_OP_RETURN_PKSCRIPT, Brc20IndexerConfig, NO_WALLET, OP_RETURN},
    database::{
        Brc20Balance, TransferValidity, get_brc20_database,
        timer::{start_timer, stop_timer},
    },
    types::{
        Ticker,
        events::{
            Brc20ProgCallTransferEvent, Brc20ProgDeployTransferEvent,
            Brc20ProgTransactTransferEvent, Brc20ProgWithdrawTransferEvent, DeployInscribeEvent,
            Event, MintInscribeEvent, TransferInscribeEvent, TransferTransferEvent,
        },
    },
};

static SPAN: &str = "EventProcessor";

pub struct EventProcessor;

impl EventProcessor {
    pub async fn brc20_prog_deploy_inscribe(
        block_height: i32,
        inscription_id: &str,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_deploy_inscribe", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_deploy_transfer(
        prog_client: &HttpClient,
        block_height: i32,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        inscription_id: &str,
        event: &Brc20ProgDeployTransferEvent,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_deploy_transfer", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Used);

        prog_client
            .brc20_deploy(
                event.source_pk_script.clone(),
                event.data.as_ref().map(|d| RawBytes::new(d.to_string())),
                event
                    .base64_data
                    .as_ref()
                    .map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
                Some(event.byte_len as u64),
            )
            .await
            .expect("Failed to deploy smart contract, please check your brc20_prog node");

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_call_inscribe(
        block_height: i32,
        inscription_id: &str,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_call_inscribe", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_call_transfer(
        prog_client: &HttpClient,
        block_height: i32,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        inscription_id: &str,
        event: &Brc20ProgCallTransferEvent,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_call_transfer", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Used);

        let contract_address = if let Some(address) = event.contract_address.clone() {
            Some(address.as_str().try_into()?)
        } else {
            None
        };

        prog_client
            .brc20_call(
                event.source_pk_script.clone(),
                contract_address,
                event
                    .contract_inscription_id
                    .as_ref()
                    .map(|s| s.to_string()),
                event.data.as_ref().map(|d| RawBytes::new(d.to_string())),
                event
                    .base64_data
                    .as_ref()
                    .map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
                Some(event.byte_len as u64),
            )
            .await
            .expect("Failed to call smart contract, please check your brc20_prog node");

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_transact_inscribe(
        block_height: i32,
        inscription_id: &str,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_transact_inscribe", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_transact_transfer(
        prog_client: &HttpClient,
        block_height: i32,
        block_hash: &str,
        inscription_id: &str,
        block_time: u64,
        brc20_prog_tx_idx: u64,
        event: &Brc20ProgTransactTransferEvent,
    ) -> Result<u64, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_transact_transfer", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Used);

        let transact_result = prog_client
            .brc20_transact(
                event.data.as_ref().map(|d| RawBytes::new(d.to_string())),
                event
                    .base64_data
                    .as_ref()
                    .map(|b| Base64Bytes::new(b.to_string())),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
                Some(event.byte_len as u64),
            )
            .await
            .expect("Failed to run transact, please check your brc20_prog node");

        stop_timer(&function_timer).await;
        Ok(transact_result.len() as u64)
    }

    pub async fn brc20_prog_withdraw_inscribe(
        block_height: i32,
        inscription_id: &str,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_withdraw_inscribe", block_height);
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Valid);
        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_prog_withdraw_transfer(
        prog_client: &HttpClient,
        block_height: i32,
        block_hash: &str,
        block_time: u64,
        brc20_prog_tx_idx: u64,
        inscription_id: &str,
        event_id: i64,
        event: &Brc20ProgWithdrawTransferEvent,
    ) -> Result<bool, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_withdraw_transfer", block_height);
        if event
            .spent_pk_script
            .as_ref()
            .and_then(|s| Some(s.starts_with(OP_RETURN)))
            .unwrap_or(false)
        {
            get_brc20_database()
                .lock()
                .await
                .set_transfer_validity(inscription_id, TransferValidity::Invalid);
            tracing::debug!(
                "New pkscript is OP_RETURN for withdraw inscription ID: {}",
                inscription_id
            );
            return Ok(false);
        }

        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(inscription_id, TransferValidity::Used);

        let withdraw_result = prog_client
            .brc20_withdraw(
                event.source_pk_script.clone(),
                event.ticker.clone(),
                event.amount.into(),
                block_time,
                block_hash.try_into()?,
                brc20_prog_tx_idx,
                Some(inscription_id.to_string()),
            )
            .await
            .expect("Failed to run withdraw, please check your brc20_prog node");

        if !withdraw_result.status.is_zero() {
            let mut brc20_prog_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&event.ticker, &BRC20_PROG_OP_RETURN_PKSCRIPT)
                .await?;

            brc20_prog_balance.overall_balance -= event.amount;
            brc20_prog_balance.available_balance -= event.amount;

            // Reduce balance in the BRC20PROG module
            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                &BRC20_PROG_OP_RETURN_PKSCRIPT,
                NO_WALLET,
                &Brc20Balance {
                    overall_balance: brc20_prog_balance.overall_balance,
                    available_balance: brc20_prog_balance.available_balance,
                },
                block_height,
                event_id,
            )?;

            let Some(target_pkscript) = event.spent_pk_script.as_ref() else {
                tracing::debug!("Target pk script not found");
                return Err("Target pk script not found")?;
            };

            let mut target_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&event.ticker, target_pkscript)
                .await?;

            target_balance.overall_balance += event.amount;
            target_balance.available_balance += event.amount;

            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                target_pkscript,
                event
                    .spent_wallet
                    .as_ref()
                    .unwrap_or(&NO_WALLET.to_string()),
                &Brc20Balance {
                    overall_balance: target_balance.overall_balance,
                    available_balance: target_balance.available_balance,
                },
                block_height,
                -event_id, // Negate to create a unique event ID
            )?;
        }

        stop_timer(&function_timer).await;
        Ok(true)
    }

    pub async fn brc20_deploy_inscribe(
        block_height: i32,
        inscription_id: &str,
        event: &DeployInscribeEvent,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_deploy_inscribe", block_height);

        let new_ticker = Ticker {
            ticker: event.ticker.to_string(),
            remaining_supply: event.max_supply,
            limit_per_mint: event.limit_per_mint,
            decimals: event.decimals,
            is_self_mint: event.is_self_mint,
            deploy_inscription_id: inscription_id.to_string(),
            original_ticker: event.original_ticker.to_string(),
            _max_supply: event.max_supply,
            burned_supply: 0,
            deploy_block_height: block_height,
        };
        get_brc20_database().lock().await.add_ticker(&new_ticker)?;

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_mint_inscribe(
        block_height: i32,
        event_id: i64,
        event: &MintInscribeEvent,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_mint_inscribe", block_height);

        let mut deployed_ticker = get_brc20_database()
            .lock()
            .await
            .get_ticker(&event.ticker)?
            .ok_or("Ticker not found")?;

        deployed_ticker.remaining_supply -= event.amount;

        get_brc20_database()
            .lock()
            .await
            .update_ticker(deployed_ticker.clone())?;

        let mut balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&deployed_ticker.ticker, &event.minted_pk_script)
            .await?;

        balance.overall_balance += event.amount;
        balance.available_balance += event.amount;

        get_brc20_database().lock().await.update_balance(
            deployed_ticker.ticker.as_str(),
            &event.minted_pk_script,
            &event.minted_wallet,
            &balance,
            block_height,
            event_id,
        )?;

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_transfer_inscribe(
        block_height: i32,
        event_id: i64,
        inscription_id: &str,
        event: &TransferInscribeEvent,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_transfer_inscribe", block_height);

        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&inscription_id, TransferValidity::Valid);

        let mut balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&event.ticker, &event.source_pk_script)
            .await?;

        if balance.available_balance < event.amount {
            tracing::error!(
                "Insufficient balance for transfer {}: available {}, required {}",
                event.ticker,
                balance.available_balance,
                event.amount
            );
            return Err("Insufficient balance for transfer")?;
        }

        balance.available_balance -= event.amount;

        get_brc20_database().lock().await.update_balance(
            &event.ticker,
            &event.source_pk_script,
            &event.source_wallet,
            &balance,
            block_height,
            event_id,
        )?;

        stop_timer(&function_timer).await;
        Ok(())
    }

    pub async fn brc20_transfer_transfer(
        prog_client: &HttpClient,
        block_height: i32,
        block_time: u64,
        block_hash: &str,
        brc20_prog_tx_idx: u64,
        inscription_id: &str,
        event_id: i64,
        event: &TransferTransferEvent,
        config: &Brc20IndexerConfig,
    ) -> Result<(), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_transfer_transfer", block_height);

        let TransferValidity::Valid = get_brc20_database()
            .lock()
            .await
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
            return Ok(());
        };

        // TODO: Reduce overall balance of the source wallet

        // If sent as fee, return to the source wallet
        // If sent to BRC20_PROG_OP_RETURN_PKSCRIPT, send to brc20_prog
        // If sent to OP_RETURN, update burned supply
        // If sent to a wallet, update the wallet balance
        let mut source_balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&event.ticker, &event.source_pk_script)
            .await?;
        let spent_pk_script = event.spent_pk_script.clone().unwrap_or_default();
        if spent_pk_script.is_empty() {
            source_balance.available_balance += event.amount;

            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                &event.source_pk_script,
                &event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;
        } else if spent_pk_script == BRC20_PROG_OP_RETURN_PKSCRIPT {
            if (block_height < config.first_brc20_prog_all_tickers_height
                && event.original_ticker.as_bytes().len() < 6)
                || block_height < config.first_brc20_prog_phase_one_height
                || !config.brc20_prog_enabled
            {
                // Burn tokens if BRC20 Prog is not enabled for this ticker yet
                source_balance.overall_balance -= event.amount;
                get_brc20_database().lock().await.update_balance(
                    &event.ticker,
                    &event.source_pk_script,
                    &event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

                let Some(mut ticker) = get_brc20_database()
                    .lock()
                    .await
                    .get_ticker(&event.ticker)?
                else {
                    tracing::debug!("Ticker not found for {}", event.ticker);
                    return Err("Ticker not found")?;
                };

                ticker.burned_supply += event.amount;
                get_brc20_database()
                    .lock()
                    .await
                    .update_ticker(ticker.clone())?;

                tracing::warn!(
                    "Burning {} tokens for ticker {} as BRC20 Prog is not enabled yet",
                    event.amount,
                    event.ticker
                );
                return Ok(());
            } else {
                source_balance.overall_balance -= event.amount;

                get_brc20_database().lock().await.update_balance(
                    &event.ticker,
                    &event.source_pk_script,
                    &event.source_wallet,
                    &source_balance,
                    block_height,
                    event_id,
                )?;

                let mut brc20_prog_balance = get_brc20_database()
                    .lock()
                    .await
                    .get_balance(&event.ticker, BRC20_PROG_OP_RETURN_PKSCRIPT)
                    .await?;

                brc20_prog_balance.available_balance += event.amount;
                brc20_prog_balance.overall_balance += event.amount;

                get_brc20_database().lock().await.update_balance(
                    &event.ticker,
                    BRC20_PROG_OP_RETURN_PKSCRIPT,
                    NO_WALLET,
                    &brc20_prog_balance,
                    block_height,
                    -event_id, // Negate to create a unique event ID
                )?;
                prog_client
                    .brc20_deposit(
                        event.source_pk_script.clone(),
                        event.ticker.clone(),
                        event.amount.into(),
                        block_time,
                        block_hash.try_into()?,
                        brc20_prog_tx_idx,
                        Some(inscription_id.to_string()),
                    )
                    .await?;
            }
        } else if spent_pk_script == OP_RETURN {
            let mut source_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&event.ticker, &event.source_pk_script)
                .await?;

            source_balance.overall_balance -= event.amount;

            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                &event.source_pk_script,
                &event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;

            let Some(mut ticker) = get_brc20_database()
                .lock()
                .await
                .get_ticker(&event.ticker)?
            else {
                tracing::debug!("Ticker not found for {}", event.ticker);
                return Err("Ticker not found")?;
            };
            // Update burned supply
            ticker.burned_supply += event.amount;
            get_brc20_database()
                .lock()
                .await
                .update_ticker(ticker.clone())?;
        } else {
            let mut source_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&event.ticker, &event.source_pk_script)
                .await?;

            source_balance.overall_balance -= event.amount;

            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                &event.source_pk_script,
                &event.source_wallet,
                &source_balance,
                block_height,
                event_id,
            )?;

            let mut target_balance = get_brc20_database()
                .lock()
                .await
                .get_balance(&event.ticker, &spent_pk_script)
                .await?;

            target_balance.available_balance += event.amount;
            target_balance.overall_balance += event.amount;

            get_brc20_database().lock().await.update_balance(
                &event.ticker,
                &spent_pk_script,
                &event.spent_wallet.clone().unwrap_or(NO_WALLET.to_string()),
                &target_balance,
                block_height,
                -event_id, // Negate to create a unique event ID
            )?;
        }
        get_brc20_database()
            .lock()
            .await
            .set_transfer_validity(&inscription_id, TransferValidity::Used);

        stop_timer(&function_timer).await;
        Ok(())
    }
}
