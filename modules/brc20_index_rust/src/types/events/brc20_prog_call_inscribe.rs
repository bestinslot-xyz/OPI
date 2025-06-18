use serde::{Deserialize, Serialize};

use super::Event;

#[derive(Serialize, Deserialize, Debug)]

pub struct Brc20ProgCallInscribeEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    pub contract_address: String,
    pub contract_inscription_id: String,
    pub data: String,
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
            "{};{};{};{};{};{}",
            Self::event_name(),
            inscription_id,
            self.source_pk_script,
            self.contract_address,
            self.contract_inscription_id,
            self.data
        )
    }
}
