use crate::types::events::{
    Brc20ProgCallInscribeEvent, Brc20ProgCallTransferEvent, Brc20ProgDeployInscribeEvent,
    Brc20ProgDeployTransferEvent, Brc20ProgTransactInscribeEvent, Brc20ProgTransactTransferEvent,
    Brc20ProgWithdrawInscribeEvent, Brc20ProgWithdrawTransferEvent, DeployInscribeEvent,
    MintInscribeEvent, PreDeployInscribeEvent, TransferInscribeEvent, TransferTransferEvent,
};

pub trait Event {
    fn event_name() -> String;
    fn event_id() -> i32;
    fn get_event_str(&self, inscription_id: &str, decimals: u8) -> String;
}

pub fn number_string_with_full_decimals(number: u128, decimals: u8) -> String {
    // Number is expected to have 18 decimals always, but we need decimals displayed correctly
    // based on the provided `decimals` parameter. Don't do multiplication or division here.
    let mut number_str = number.to_string();
    if number_str.len() <= 18 {
        // Pad with leading zeros if the number is less than 18 digits
        number_str = format!("0{:0>18}", number_str);
    }
    number_str.truncate(number_str.len() as usize - 18 + decimals as usize);
    if decimals > 0 {
        // Insert the decimal point
        let insert_index = number_str.len() - decimals as usize;
        number_str.insert(insert_index, '.');
    }

    number_str
}

pub fn load_event<T>(event_type_id: i32, event_record: &serde_json::Value) -> Result<T, String>
where
    T: Event + serde::de::DeserializeOwned,
{
    if event_type_id == T::event_id() {
        serde_json::from_value(event_record.clone())
            .map_err(|e| format!("Failed to deserialize event: {}", e))
    } else {
        Err(format!(
            "Event type ID {} does not match expected ID {}",
            event_type_id,
            T::event_id()
        ))
    }
}

pub fn event_name_to_id(event_name: &str) -> i32 {
    if event_name == Brc20ProgCallInscribeEvent::event_name() {
        Brc20ProgCallInscribeEvent::event_id()
    } else if event_name == Brc20ProgCallTransferEvent::event_name() {
        Brc20ProgCallTransferEvent::event_id()
    } else if event_name == Brc20ProgDeployInscribeEvent::event_name() {
        Brc20ProgDeployInscribeEvent::event_id()
    } else if event_name == Brc20ProgDeployTransferEvent::event_name() {
        Brc20ProgDeployTransferEvent::event_id()
    } else if event_name == Brc20ProgTransactInscribeEvent::event_name() {
        Brc20ProgTransactInscribeEvent::event_id()
    } else if event_name == Brc20ProgTransactTransferEvent::event_name() {
        Brc20ProgTransactTransferEvent::event_id()
    } else if event_name == Brc20ProgWithdrawInscribeEvent::event_name() {
        Brc20ProgWithdrawInscribeEvent::event_id()
    } else if event_name == Brc20ProgWithdrawTransferEvent::event_name() {
        Brc20ProgWithdrawTransferEvent::event_id()
    } else if event_name == PreDeployInscribeEvent::event_name() {
        PreDeployInscribeEvent::event_id()
    } else if event_name == DeployInscribeEvent::event_name() {
        DeployInscribeEvent::event_id()
    } else if event_name == MintInscribeEvent::event_name() {
        MintInscribeEvent::event_id()
    } else if event_name == TransferInscribeEvent::event_name() {
        TransferInscribeEvent::event_id()
    } else if event_name == TransferTransferEvent::event_name() {
        TransferTransferEvent::event_id()
    } else {
        -1 // Unknown event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_number_string_with_full_decimals() {
        assert_eq!(
            number_string_with_full_decimals(200000000000000000, 18),
            "0.200000000000000000"
        );
        assert_eq!(
            number_string_with_full_decimals(21000000000000000000000000, 8),
            "21000000.00000000"
        );
        assert_eq!(
            number_string_with_full_decimals(1234567890000000000000000, 2),
            "1234567.89"
        );
        assert_eq!(
            number_string_with_full_decimals(12300000000000000000, 5),
            "12.30000"
        );
        assert_eq!(
            number_string_with_full_decimals(12345678000000000000000000, 0),
            "12345678"
        );
        assert_eq!(number_string_with_full_decimals(0, 3), "0.000");
        assert_eq!(number_string_with_full_decimals(0, 0), "0");
    }
}
