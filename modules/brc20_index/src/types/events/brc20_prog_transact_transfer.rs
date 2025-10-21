use serde::{Deserialize, Serialize};
use serde_with::{DefaultOnNull, serde_as};

use crate::types::events::event::get_wallet_from_pk_script;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct Brc20ProgTransactTransferEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub source_wallet: String,
    #[serde(rename = "spent_pkScript")]
    pub spent_pk_script: String,
    pub data: Option<String>,
    pub base64_data: Option<String>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub byte_len: i32,
    #[serde(rename = "btc_txId")]
    pub btc_txid: Option<String>,
}

impl Event for Brc20ProgTransactTransferEvent {
    fn event_name() -> String {
        "brc20prog-transact-transfer".to_string()
    }

    fn event_id() -> i32 {
        11
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        if let Some(btc_txid) = &self.btc_txid {
            return format!(
                "{};{};{};{};{};{};{};{}",
                Self::event_name(),
                inscription_id,
                self.source_pk_script,
                self.spent_pk_script,
                self.data.clone().unwrap_or_default(),
                self.base64_data.clone().unwrap_or_default(),
                self.byte_len,
                btc_txid
            );
        } else {
            format!(
                "{};{};{};{};{};{};{}",
                Self::event_name(),
                inscription_id,
                self.source_pk_script,
                self.spent_pk_script,
                self.data.clone().unwrap_or_default(),
                self.base64_data.clone().unwrap_or_default(),
                self.byte_len
            )
        }
    }

    fn calculate_wallets(&mut self, network: bitcoin::Network) {
        if let Some(wallet) = get_wallet_from_pk_script(&self.source_pk_script, network) {
            self.source_wallet = wallet;
        } else {
            self.source_wallet = String::new();
        }
    }
}
