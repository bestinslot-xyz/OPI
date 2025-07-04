use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct Brc20ProgTransactTransferEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    #[serde(rename = "spent_pkScript")]
    pub spent_pk_script: String,
    pub data: Option<String>,
    pub base64_data: Option<String>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub byte_len: i32,
}

impl Event for Brc20ProgTransactTransferEvent {
    fn event_name() -> String {
        "brc20prog-transact-transfer".to_string()
    }

    fn event_id() -> i32 {
        11
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
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
