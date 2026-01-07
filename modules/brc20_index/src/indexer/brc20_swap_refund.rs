use std::{error::Error, str::FromStr};

use db_reader::BRC20Tx;
use jsonrpsee::http_client::HttpClient;
use serde_json::json;

use crate::{
    config::{Brc20IndexerConfig, NO_WALLET},
    database::get_brc20_database,
    indexer::{EventGenerator, EventProcessor},
};

const EMPTY_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct Brc20SwapRefund;

impl Brc20SwapRefund {
    pub async fn generate_and_process_refunds(
        block_height: i32,
        brc20_prog_client: &HttpClient,
        config: &Brc20IndexerConfig,
        block_time: u64,
        block_hash: &str,
    ) -> Result<(), Box<dyn Error>> {
        tracing::info!(
            "Generating and processing BRC-20 swap refunds at block height {}",
            block_height
        );
        // Implementation for processing BRC-20 swap refunds
        let balances = get_brc20_database()
            .lock()
            .await
            .get_balance_all_tickers(&config.brc20_swap_module_pkscript)
            .await?;

        for (ticker, balance) in balances {
            tracing::info!(
                "Processing refund for ticker {}: balance {}",
                ticker,
                balance.overall_balance
            );
            let ticker = get_brc20_database()
                .lock()
                .await
                .get_ticker(ticker.as_str())?
                .expect("Ticker not found");

            let inscription_id = get_inscription_id_for_ticker_refund_hex(&ticker.ticker);
            let using_tx_id = format!("{}:0", block_height);

            if balance.overall_balance > 0 {
                let (inscribe_event_id, inscribe_event) = EventGenerator::brc20_transfer_inscribe(
                    block_height,
                    &ticker,
                    &ticker.original_ticker,
                    balance.overall_balance,
                    &BRC20Tx {
                        tx_id: using_tx_id.clone(),
                        inscription_id: inscription_id.clone(),
                        inscription_number: i32::MAX,
                        old_satpoint: None,
                        new_satpoint: "".to_string(),
                        txid: EMPTY_TXID.to_string(),
                        new_pkscript: config.brc20_swap_module_pkscript.clone(),
                        new_wallet: NO_WALLET.to_string(),
                        sent_as_fee: false,
                        content: json!({
                            "p": "brc-20",
                            "op": "transfer",
                            "tick": ticker.ticker,
                            "amt": balance.overall_balance,
                        }),
                        byte_len: 0,
                        parent_id: None,
                    },
                )
                .await?;

                EventProcessor::brc20_transfer_inscribe(
                    block_height,
                    inscribe_event_id,
                    inscription_id.as_str(),
                    &inscribe_event,
                )
                .await?;

                let (transfer_event_id, transfer_event) = EventGenerator::brc20_transfer_transfer(
                    block_height,
                    &ticker,
                    &ticker.original_ticker.as_str(),
                    balance.overall_balance,
                    &BRC20Tx {
                        tx_id: using_tx_id.clone(),
                        inscription_id: inscription_id.clone(),
                        inscription_number: i32::MAX,
                        old_satpoint: None,
                        new_satpoint: "".to_string(),
                        txid: EMPTY_TXID.to_string(),
                        new_pkscript: bitcoin::Address::from_str(
                            &config.brc20_swap_refund_address,
                        )?
                        .require_network(config.network_type)?
                        .script_pubkey()
                        .to_hex_string(),
                        new_wallet: config.brc20_swap_refund_address.to_string(),
                        sent_as_fee: false,
                        content: json!({
                            "p": "brc-20",
                            "op": "transfer",
                            "tick": ticker.ticker,
                            "amt": balance.overall_balance,
                        }),
                        byte_len: 0,
                        parent_id: None,
                    },
                )
                .await?;

                EventProcessor::brc20_transfer_transfer(
                    brc20_prog_client,
                    block_height,
                    block_time,
                    &block_hash,
                    0,
                    &get_inscription_id_for_ticker_refund_hex(&ticker.ticker),
                    transfer_event_id,
                    &transfer_event,
                    config,
                )
                .await?;
            }
        }

        Ok(())
    }
}

pub fn get_inscription_id_for_ticker_refund_hex(ticker: &str) -> String {
    let mut inscription_id = format!(
        "{}{}",
        hex::encode("BRC20SWAPREFUND".as_bytes()),
        hex::encode(ticker.as_bytes())
    );
    while inscription_id.len() < 64 {
        inscription_id.insert(inscription_id.len(), '0');
    }
    format!("{}i0", inscription_id)
}
