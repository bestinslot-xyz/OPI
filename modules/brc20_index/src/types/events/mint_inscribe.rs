use serde::{Deserialize, Serialize};
use serde_with::{DefaultOnNull, serde_as};

use crate::types::events::{event::get_wallet_from_pk_script, number_string_with_full_decimals};

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct MintInscribeEvent {
    #[serde(rename = "minted_pkScript")]
    pub minted_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub minted_wallet: String,
    #[serde(rename = "tick")]
    pub ticker: String,
    #[serde(rename = "original_tick")]
    pub original_ticker: String,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub amount: u128,
    pub parent_id: String,
}

impl Event for MintInscribeEvent {
    fn event_name() -> String {
        "mint-inscribe".to_string()
    }

    fn event_id() -> i32 {
        1
    }

    fn get_event_str(&self, inscription_id: &str, decimals: u8) -> String {
        format!(
            "{};{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.minted_pk_script,
            self.ticker,
            self.original_ticker,
            number_string_with_full_decimals(self.amount, decimals),
            self.parent_id
        )
    }

    fn calculate_wallets(&mut self, network: bitcoin::Network) {
        if let Some(wallet) = get_wallet_from_pk_script(&self.minted_pk_script, network) {
            self.minted_wallet = wallet;
        } else {
            self.minted_wallet = String::new();
        }
    }
}
