use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DefaultOnNull};

use crate::types::events::number_string_with_full_decimals;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct DeployInscribeEvent {
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub decimals: u8,
    #[serde(rename = "deployer_pkScript")]
    pub deployer_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub deployer_wallet: String,
    #[serde(rename = "tick")]
    pub ticker: String,
    #[serde(rename = "original_tick")]
    pub original_ticker: String,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub max_supply: u128,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub limit_per_mint: u128,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub is_self_mint: bool,
}

impl Event for DeployInscribeEvent {
    fn event_name() -> String {
        "deploy-inscribe".to_string()
    }

    fn event_id() -> i32 {
        0
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        format!(
            "{};{};{};{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.deployer_pk_script,
            self.ticker,
            self.original_ticker,
            number_string_with_full_decimals(self.max_supply, self.decimals),
            self.decimals,
            number_string_with_full_decimals(self.limit_per_mint, self.decimals),
            self.is_self_mint
        )
    }
}
