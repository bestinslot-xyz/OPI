mod event;
pub use event::{Event, number_string_with_full_decimals};

mod deploy_inscribe;
pub use deploy_inscribe::DeployInscribeEvent;

mod mint_inscribe;
pub use mint_inscribe::MintInscribeEvent;

mod transfer_inscribe;
pub use transfer_inscribe::TransferInscribeEvent;

mod transfer_transfer;
pub use transfer_transfer::TransferTransferEvent;

mod brc20_prog_deploy_inscribe;
pub use brc20_prog_deploy_inscribe::Brc20ProgDeployInscribeEvent;

mod brc20_prog_deploy_transfer;
pub use brc20_prog_deploy_transfer::Brc20ProgDeployTransferEvent;

mod brc20_prog_call_inscribe;
pub use brc20_prog_call_inscribe::Brc20ProgCallInscribeEvent;

mod brc20_prog_call_transfer;
pub use brc20_prog_call_transfer::Brc20ProgCallTransferEvent;

mod brc20_prog_withdraw_inscribe;
pub use brc20_prog_withdraw_inscribe::Brc20ProgWithdrawInscribeEvent;

mod brc20_prog_withdraw_transfer;
pub use brc20_prog_withdraw_transfer::Brc20ProgWithdrawTransferEvent;

pub fn get_event_str_by_id_and_json_value(
    inscription_id: &str,
    event_id: i32,
    json_value: serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    match event_id {
        0 => {
            let event = serde_json::from_value::<DeployInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        1 => {
            let event = serde_json::from_value::<MintInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        2 => {
            let event = serde_json::from_value::<TransferInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        3 => {
            let event = serde_json::from_value::<TransferTransferEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        4 => {
            let event = serde_json::from_value::<Brc20ProgDeployInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        5 => {
            let event = serde_json::from_value::<Brc20ProgDeployTransferEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        6 => {
            let event = serde_json::from_value::<Brc20ProgCallInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        7 => {
            let event = serde_json::from_value::<Brc20ProgCallTransferEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        8 => {
            let event = serde_json::from_value::<Brc20ProgWithdrawInscribeEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        9 => {
            let event = serde_json::from_value::<Brc20ProgWithdrawTransferEvent>(json_value)?;
            Ok(event.get_event_str(inscription_id, 0))
        }
        _ => Err(format!("Unknown event ID: {}", event_id).into()),
    }
}
