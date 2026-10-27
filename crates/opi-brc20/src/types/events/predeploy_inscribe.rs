use serde::{Deserialize, Serialize};
use serde_with::{DefaultOnNull, serde_as};

use crate::types::events::event::get_wallet_from_pk_script;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct PreDeployInscribeEvent {
    #[serde(rename = "predeployer_pkScript")]
    pub predeployer_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub predeployer_wallet: String,
    pub hash: String,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub block_height: i32,
}

impl Event for PreDeployInscribeEvent {
    fn event_name() -> String {
        "predeploy-inscribe".to_string()
    }

    fn event_id() -> i32 {
        12
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        format!(
            "{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.predeployer_pk_script,
            self.hash,
            self.block_height,
        )
    }

    fn calculate_wallets(&mut self, network: bitcoin::Network) {
        if let Some(wallet) = get_wallet_from_pk_script(&self.predeployer_pk_script, network) {
            self.predeployer_wallet = wallet;
        } else {
            self.predeployer_wallet = String::new();
        }
    }
}
