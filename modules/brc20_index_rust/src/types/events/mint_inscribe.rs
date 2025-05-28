use serde::{Deserialize, Serialize};

use crate::types::events::number_string_with_full_decimals;

use super::Event;

#[derive(Serialize, Deserialize, Debug)]

pub struct MintInscribeEvent {
    #[serde(rename = "minted_pkScript")]
    pub minted_pk_script: String,
    #[serde(default)]
    pub minted_wallet: String,
    #[serde(rename = "tick")]
    pub ticker: String,
    #[serde(rename = "original_tick")]
    pub original_ticker: String,
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
}
