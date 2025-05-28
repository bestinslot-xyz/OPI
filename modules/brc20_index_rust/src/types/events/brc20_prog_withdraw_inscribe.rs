use serde::{Deserialize, Serialize};

use crate::types::events::number_string_with_full_decimals;

use super::Event;

#[derive(Serialize, Deserialize, Debug)]
pub struct Brc20ProgWithdrawInscribeEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    pub source_wallet: String,
    #[serde(rename = "tick")]
    pub ticker: String,
    #[serde(rename = "original_tick")]
    pub original_ticker: String,
    pub amount: u128,
}

impl Event for Brc20ProgWithdrawInscribeEvent {
    fn event_name() -> String {
        "brc20prog-withdraw-inscribe".to_string()
    }

    fn event_id() -> i32 {
        8
    }

    fn get_event_str(&self, inscription_id: &str, decimals: u8) -> String {
        format!(
            "{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.source_pk_script,
            self.ticker,
            self.original_ticker,
            number_string_with_full_decimals(self.amount, decimals)
        )
    }
}
