use std::error::Error;

use db_reader::BRC20Tx;

use crate::{
    config::BRC20_PROG_OP_RETURN_PKSCRIPT,
    database::{
        TransferValidity, get_brc20_database,
        timer::{start_timer, stop_timer},
    },
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

static SPAN: &str = "EventGenerator";

pub struct EventGenerator;

impl EventGenerator {
    pub async fn brc20_prog_deploy_inscribe(
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgDeployInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_deploy_inscribe", block_height);
        let event = Brc20ProgDeployInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string().into(),
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
            None,
        )?;
        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_deploy_transfer(
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgDeployTransferEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_deploy_transfer", block_height);
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
            source_wallet: inscribe_event.source_wallet.to_string(),
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
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_call_transfer(
        block_height: i32,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgCallTransferEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_call_transfer", block_height);
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

        let event = Brc20ProgCallTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.to_string(),
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
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_call_inscribe(
        block_height: i32,
        contract_address: Option<&str>,
        contract_inscription_id: Option<&str>,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgCallInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_call_inscribe", block_height);
        let event = Brc20ProgCallInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string().into(),
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
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_transact_inscribe(
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgTransactInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_transact_inscribe", block_height);
        let event = Brc20ProgTransactInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string().into(),
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
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_transact_transfer(
        block_height: i32,
        data: Option<&str>,
        base64_data: Option<&str>,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgTransactTransferEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_transact_transfer", block_height);
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

        let event = Brc20ProgTransactTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.to_string(),
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
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_prog_withdraw_transfer(
        block_height: i32,
        deployed_ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        transfer: &BRC20Tx,
    ) -> Result<(i64, Brc20ProgWithdrawTransferEvent), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_withdraw_transfer", block_height);
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

        let event = Brc20ProgWithdrawTransferEvent {
            source_pk_script: inscribe_event.source_pk_script.clone(),
            source_wallet: inscribe_event.source_wallet.clone(),
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
            ticker: deployed_ticker.ticker.clone(),
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
            Some(deployed_ticker.decimals),
        )?;

        stop_timer(&function_timer).await;
        Ok((event_id, event))
    }

    pub async fn brc20_prog_withdraw_inscribe(
        block_height: i32,
        deployed_ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        transfer: &BRC20Tx,
    ) -> Result<Brc20ProgWithdrawInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_prog_withdraw_inscribe", block_height);
        let event = Brc20ProgWithdrawInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string().into(),
            ticker: deployed_ticker.ticker.to_string(),
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
            Some(deployed_ticker.decimals),
        )?;
        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_predeploy_inscribe(
        block_height: i32,
        hash: &str,
        transfer: &BRC20Tx,
    ) -> Result<PreDeployInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_predeploy_inscribe", block_height);
        let predeploy_event = PreDeployInscribeEvent {
            predeployer_pk_script: transfer.new_pkscript.clone(),
            predeployer_wallet: transfer.new_wallet.clone(),
            hash: hash.to_string(),
            block_height: block_height,
        };

        get_brc20_database().lock().await.add_event(
            block_height,
            &transfer.inscription_id,
            &transfer.inscription_number,
            &transfer.old_satpoint,
            &transfer.new_satpoint,
            &transfer.txid,
            &predeploy_event,
            None,
        )?;

        stop_timer(&function_timer).await;
        Ok(predeploy_event)
    }

    pub async fn brc20_deploy_inscribe(
        block_height: i32,
        ticker: &str,
        original_ticker: &str,
        max_supply: u128,
        limit_per_mint: u128,
        decimals: u8,
        is_self_mint: bool,
        transfer: &BRC20Tx,
    ) -> Result<DeployInscribeEvent, Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_deploy_inscribe", block_height);
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
            Some(decimals),
        )?;

        stop_timer(&function_timer).await;
        Ok(event)
    }

    pub async fn brc20_mint_inscribe(
        block_height: i32,
        deployed_ticker: &mut Ticker,
        original_ticker: &str,
        mut amount: u128,
        transfer: &BRC20Tx,
    ) -> Result<(i64, MintInscribeEvent), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_mint_inscribe", block_height);
        if deployed_ticker.is_self_mint {
            let Some(parent_id) = transfer.parent_id.as_ref() else {
                // Skip if parent id is not present
                tracing::debug!(
                    "Skipping mint {} as parent id is not present for self-mint",
                    transfer.inscription_id
                );
                return Err("Parent id is not present")?;
            };
            if &deployed_ticker.deploy_inscription_id != parent_id {
                tracing::debug!(
                    "Skipping mint {} as parent id {} does not match deploy inscription id {}",
                    transfer.inscription_id,
                    parent_id,
                    deployed_ticker.deploy_inscription_id
                );
                return Err("Parent id does not match deploy inscription id")?;
            }
        }

        if deployed_ticker.remaining_supply == 0 {
            tracing::debug!(
                "Skipping mint {} as remaining supply is 0 for ticker {}",
                transfer.inscription_id,
                deployed_ticker.ticker
            );
            return Err("Remaining supply is 0")?;
        }

        if amount > deployed_ticker.limit_per_mint {
            tracing::debug!(
                "Skipping mint {} as amount {} exceeds limit per mint {} for ticker {}",
                transfer.inscription_id,
                amount,
                deployed_ticker.limit_per_mint,
                deployed_ticker.ticker
            );
            return Err("Amount exceeds limit per mint")?;
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
            Some(deployed_ticker.decimals),
        )?;

        stop_timer(&function_timer).await;
        Ok((event_id, event))
    }

    pub async fn brc20_transfer_inscribe(
        block_height: i32,
        deployed_ticker: &Ticker,
        original_ticker: &str,
        amount: u128,
        transfer: &BRC20Tx,
    ) -> Result<(i64, TransferInscribeEvent), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_transfer_inscribe", block_height);
        let balance = get_brc20_database()
            .lock()
            .await
            .get_balance(&deployed_ticker.ticker, &transfer.new_pkscript)
            .await?;

        // If available balance is less than amount, return early
        if balance.available_balance < amount {
            tracing::debug!(
                "Skipping transfer {} as available balance {} is less than amount {}",
                transfer.inscription_id,
                balance.available_balance,
                amount
            );
            return Err("Available balance is less than amount")?;
        }

        let event = TransferInscribeEvent {
            source_pk_script: transfer.new_pkscript.to_string(),
            source_wallet: transfer.new_wallet.to_string(),
            ticker: deployed_ticker.ticker.to_string(),
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
            Some(deployed_ticker.decimals),
        )?;

        stop_timer(&function_timer).await;
        Ok((event_id, event))
    }

    pub async fn brc20_transfer_transfer(
        block_height: i32,
        ticker: &mut Ticker,
        original_ticker: &str,
        amount: u128,
        transfer: &BRC20Tx,
    ) -> Result<(i64, TransferTransferEvent), Box<dyn Error>> {
        let function_timer = start_timer(SPAN, "brc20_transfer_transfer", block_height);
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
            Some(ticker.decimals),
        )?;

        stop_timer(&function_timer).await;
        Ok((event_id, event))
    }
}
