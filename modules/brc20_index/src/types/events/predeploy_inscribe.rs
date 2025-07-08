use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct PreDeployInscribeEvent {
    #[serde(rename = "deployer_pkScript")]
    pub deployer_pk_script: String,
    pub deployer_wallet: String,
    pub hash: String,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub block_height: i32,
}

impl Event for PreDeployInscribeEvent {
    fn event_name() -> String {
        "pre-deploy-inscribe".to_string()
    }

    fn event_id() -> i32 {
        12
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        format!(
            "{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.deployer_pk_script,
            self.hash,
            self.block_height,
        )
    }
}
