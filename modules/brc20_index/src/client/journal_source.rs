use std::{collections::HashMap, error::Error, sync::Arc};

use bitcoin::Network;
use db_reader::BRC20Tx;
use opi_core::{BlockSource, InscriptionInfo};

use super::Brc20Source;

/// Serves BRC-20 block data from an in-process [`BlockSource`], producing the
/// same `BRC20Tx` values the fork's `db_reader` returns over JSON-RPC.
///
/// Each read opens its own journal read transaction, so the reads that make
/// up one block are not one snapshot. A reorg between two of them surfaces
/// as a retryable error, and the supervisor's restart of the indexer heals
/// it.
pub struct JournalBrc20Source {
    source: Arc<dyn BlockSource>,
    network: Network,
}

impl JournalBrc20Source {
    pub fn new(source: Arc<dyn BlockSource>, network: Network) -> Self {
        JournalBrc20Source { source, network }
    }
}

#[async_trait::async_trait]
impl Brc20Source for JournalBrc20Source {
    async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        self.source
            .tip()?
            .map(|height| height as i32)
            .ok_or("Failed to get current block height".into())
    }

    async fn get_block_hash(&self, block_height: i32) -> Result<String, Box<dyn Error>> {
        self.source
            .block_info(block_height as u32)?
            .map(|info| info.hash)
            .ok_or("Block hash not found".into())
    }

    async fn get_block_hash_and_time(
        &self,
        block_height: i32,
    ) -> Result<(String, i64), Box<dyn Error>> {
        self.source
            .block_info(block_height as u32)?
            .map(|info| (info.hash, info.time as i64))
            .ok_or("Block hash and time not found".into())
    }

    async fn get_transfers(&self, block_height: i32) -> Result<Vec<BRC20Tx>, Box<dyn Error>> {
        // Per inscription id: its info when valid for BRC-20, `None` when rejected.
        let mut verdicts: HashMap<String, Option<InscriptionInfo>> = HashMap::new();
        let mut txes = Vec::new();

        for record in self.source.inscription_records(block_height as u32)? {
            if !verdicts.contains_key(&record.inscription_id) {
                let info = self
                    .source
                    .inscription_info(&record.inscription_id)?
                    .ok_or_else(|| {
                        format!("Inscription info not found for {}", record.inscription_id)
                    })?;
                let verdict = is_valid_brc20(&info).then_some(info);
                verdicts.insert(record.inscription_id.clone(), verdict);
            }
            let Some(info) = &verdicts[&record.inscription_id] else {
                continue;
            };

            let new_pkscript = hex::encode(&record.new_pkscript);
            txes.push(BRC20Tx {
                tx_id: format!("{}:{}", record.height, record.idx),
                inscription_id: record.inscription_id,
                inscription_number: info.inscription_number,
                old_satpoint: record.old_satpoint,
                new_satpoint: record.new_satpoint,
                txid: record.txid,
                new_wallet: get_wallet(&new_pkscript, self.network),
                new_pkscript,
                sent_as_fee: record.sent_as_fee,
                content: serde_json::from_slice(&info.content).unwrap_or(serde_json::Value::Null),
                byte_len: info.content.len() as u32,
                parent_id: info.parent_id.clone(),
            });
        }

        Ok(txes)
    }
}

/// Whether an inscription can carry a BRC-20, BRC2.0 program or module
/// operation.
pub fn is_valid_brc20(inscription_info: &InscriptionInfo) -> bool {
    if inscription_info.cursed_for_brc20 {
        return false;
    }
    if !inscription_info.is_json {
        return false;
    }

    let Ok(decoded_content_type) = std::str::from_utf8(&inscription_info.content_type) else {
        return false;
    };

    if !decoded_content_type.eq("text/plain")
        && !decoded_content_type.starts_with("text/plain;")
        && !decoded_content_type.eq("application/json")
        && !decoded_content_type.starts_with("application/json;")
    {
        return false;
    }

    let json_data: serde_json::Value = match serde_json::from_slice(&inscription_info.content) {
        Ok(data) => data,
        Err(_) => return false, // Invalid JSON
    };

    let Some(protocol) = json_data.get("p").and_then(|v| v.as_str()) else {
        return false; // Missing or invalid 'p' field
    };
    if protocol != "brc-20" && protocol != "brc20-prog" && protocol != "brc20-module" {
        return false;
    }
    if protocol == "brc20-module" {
        let Some(module) = json_data.get("module").and_then(|m| m.as_str()) else {
            return false; // Missing or invalid 'module' field for brc20-module
        };
        if module != "BRC20PROG" {
            return false; // Invalid module value
        }
    }

    true
}

/// Address string for a hex pkscript, or an empty string when the script has
/// no address form.
pub fn get_wallet(pkscript: &str, network: Network) -> String {
    bitcoin::Address::from_script(
        bitcoin::Script::from_bytes(&hex::decode(pkscript).unwrap()),
        network,
    )
    .map(|addr| addr.to_string())
    .unwrap_or_else(|_| "".to_string())
}
