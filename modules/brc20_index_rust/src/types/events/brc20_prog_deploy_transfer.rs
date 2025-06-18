use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct Brc20ProgDeployTransferEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    #[serde(rename = "spent_pkScript")]
    pub spent_pk_script: String,
    pub data: String,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub byte_len: i32,
}

impl Event for Brc20ProgDeployTransferEvent {
    fn event_name() -> String {
        "brc20prog-deploy-transfer".to_string()
    }

    fn event_id() -> i32 {
        5
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        format!(
            "{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.source_pk_script,
            self.spent_pk_script,
            self.data,
            self.byte_len
        )
    }
}
