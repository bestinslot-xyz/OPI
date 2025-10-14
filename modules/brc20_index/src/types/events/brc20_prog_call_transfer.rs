use serde::{Deserialize, Serialize};
use serde_with::{DefaultOnNull, serde_as};

use crate::types::events::event::get_wallet_from_pk_script;

use super::Event;

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct Brc20ProgCallTransferEvent {
    #[serde(rename = "source_pkScript")]
    pub source_pk_script: String,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub source_wallet: String,
    #[serde(rename = "spent_pkScript")]
    pub spent_pk_script: Option<String>,
    pub contract_address: Option<String>,
    pub contract_inscription_id: Option<String>,
    pub data: Option<String>,
    pub base64_data: Option<String>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub byte_len: u64,
    #[serde(rename = "btc_txId")]
    pub btc_txid: Option<String>,
}

impl Event for Brc20ProgCallTransferEvent {
    fn event_name() -> String {
        "brc20prog-call-transfer".to_string()
    }

    fn event_id() -> i32 {
        7
    }

    fn get_event_str(&self, inscription_id: &str, _decimals: u8) -> String {
        if let Some(btc_txid) = &self.btc_txid {
            return format!(
                "{};{};{};{};{};{};{};{};{};{}",
                Self::event_name(),
                inscription_id,
                self.source_pk_script,
                self.spent_pk_script.clone().unwrap_or_default(),
                self.contract_address.clone().unwrap_or_default(),
                self.contract_inscription_id.clone().unwrap_or_default(),
                self.data.clone().unwrap_or_default(),
                self.base64_data.clone().unwrap_or_default(),
                self.byte_len,
                btc_txid
            );
        } else {
            format!(
                "{};{};{};{};{};{};{};{};{}",
                Self::event_name(),
                inscription_id,
                self.source_pk_script,
                self.spent_pk_script.clone().unwrap_or_default(),
                self.contract_address.clone().unwrap_or_default(),
                self.contract_inscription_id.clone().unwrap_or_default(),
                self.data.clone().unwrap_or_default(),
                self.base64_data.clone().unwrap_or_default(),
                self.byte_len
            )
        }
    }

    fn calculate_wallets(&mut self, network: bitcoin::Network) {
        if let Some(wallet) = get_wallet_from_pk_script(&self.source_pk_script, network) {
            self.source_wallet = wallet;
        } else {
            self.source_wallet = String::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_brc20_prog_call_transfer_event_serialization() {
        let event = Brc20ProgCallTransferEvent {
            source_pk_script: "76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string(),
            source_wallet: "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string(),
            spent_pk_script: Some("76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string()),
            contract_address: Some("0x1234567890abcdef1234567890abcdef12345678".to_string()),
            contract_inscription_id: Some("inscription123".to_string()),
            data: Some("blah blah".to_string()),
            base64_data: Some(
                "dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk".to_string(),
            ),
            byte_len: 42,
            btc_txid: Some("txid123456".to_string()),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let expected = r#"{"source_pkScript":"76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac","source_wallet":"1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa","spent_pkScript":"76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac","contract_address":"0x1234567890abcdef1234567890abcdef12345678","contract_inscription_id":"inscription123","data":"blah blah","base64_data":"dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk","byte_len":"42","btc_txId":"txid123456"}"#;
        assert_eq!(serialized, expected);
        let deserialized: Brc20ProgCallTransferEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.source_pk_script, event.source_pk_script);
        assert_eq!(deserialized.source_wallet, event.source_wallet);
        assert_eq!(deserialized.spent_pk_script, event.spent_pk_script);
        assert_eq!(deserialized.contract_address, event.contract_address);
        assert_eq!(
            deserialized.contract_inscription_id,
            event.contract_inscription_id
        );
        assert_eq!(deserialized.data, event.data);
        assert_eq!(deserialized.base64_data, event.base64_data);
        assert_eq!(deserialized.byte_len, event.byte_len);
        assert_eq!(deserialized.btc_txid, event.btc_txid);
    }

    #[test]
    fn test_brc20_prog_call_transfer_event_get_event_str_no_btc_txid() {
        let event = Brc20ProgCallTransferEvent {
            source_pk_script: "76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string(),
            source_wallet: "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string(),
            spent_pk_script: Some("76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string()),
            contract_address: Some("0x1234567890abcdef1234567890abcdef12345678".to_string()),
            contract_inscription_id: Some("inscription123".to_string()),
            data: Some("blah blah".to_string()),
            base64_data: Some(
                "dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk".to_string(),
            ),
            byte_len: 42,
            btc_txid: None,
        };
        let event_str = event.get_event_str("inscription123", 0);
        let expected = "brc20prog-call-transfer;inscription123;76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac;76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac;0x1234567890abcdef1234567890abcdef12345678;inscription123;blah blah;dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk;42";
        assert_eq!(event_str, expected);
    }

    #[test]
    fn test_brc20_prog_call_transfer_event_get_event_str_with_btc_txid() {
        let event = Brc20ProgCallTransferEvent {
            source_pk_script: "76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string(),
            source_wallet: "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string(),
            spent_pk_script: Some("76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string()),
            contract_address: Some("0x1234567890abcdef1234567890abcdef12345678".to_string()),
            contract_inscription_id: Some("inscription123".to_string()),
            data: Some("blah blah".to_string()),
            base64_data: Some(
                "dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk".to_string(),
            ),
            byte_len: 42,
            btc_txid: Some("txid123456".to_string()),
        };
        let event_str = event.get_event_str("inscription123", 0);
        let expected = "brc20prog-call-transfer;inscription123;76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac;76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac;0x1234567890abcdef1234567890abcdef12345678;inscription123;blah blah;dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk;42;txid123456";
        assert_eq!(event_str, expected);
    }

    #[test]
    fn test_brc20_prog_call_transfer_event_calculate_wallets() {
        let mut event = Brc20ProgCallTransferEvent {
            source_pk_script: "76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string(),
            source_wallet: String::new(),
            spent_pk_script: Some("76a91489abcdefabbaabbaabbaabbaabbaabbaabbaabba88ac".to_string()),
            contract_address: Some("0x1234567890abcdef1234567890abcdef12345678".to_string()),
            contract_inscription_id: Some("inscription123".to_string()),
            data: Some("blah blah".to_string()),
            base64_data: Some(
                "dHJhbnNmZXIgMTAwMCB0byAweGFiY2RlZmFiY2RlZmFiY2RlZmFiY2xhYmNk".to_string(),
            ),
            byte_len: 42,
            btc_txid: Some("txid123456".to_string()),
        };
        event.calculate_wallets(bitcoin::Network::Bitcoin);
        assert_eq!(event.source_wallet, "1DYwPTpZuLjY2qApmJdHaSAuWRvEF5skCN");
    }
}
