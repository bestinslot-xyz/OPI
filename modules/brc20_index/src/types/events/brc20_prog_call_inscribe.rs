use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::types::events::event::get_wallet_from_pk_script;

use super::Event;

#[derive(Serialize, Deserialize, Debug)]
#[serde_as]
pub struct Brc20ProgCallInscribeEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub source_wallet: String,
    pub contract_address: String,
    pub contract_inscription_id: String,
    pub data: Option<String>,
    pub base64_data: Option<String>,
}

impl Event for Brc20ProgCallInscribeEvent {
    fn event_name() -> String {
        "brc20prog-call-inscribe".to_string()
    }

    fn event_id() -> i32 {
        6
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        format!(
            "{};{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.source_pk_script,
            self.contract_address,
            self.contract_inscription_id,
            self.data.clone().unwrap_or_default(),
            self.base64_data.clone().unwrap_or_default()
        )
    }

    fn calculate_wallets(&mut self, network: bitcoin::Network) {
        // Calculate the wallet address from the source_pk_script
        if let Some(wallet) = get_wallet_from_pk_script(&self.source_pk_script, network) {
            self.source_wallet = wallet;
        } else {
            self.source_wallet = String::new();
        }
    }
}
