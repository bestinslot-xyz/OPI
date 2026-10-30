//! The BRC-20 rules the journal shares with the brc20 module: which
//! inscriptions can carry a BRC-20, BRC2.0 program or module operation, and
//! which of those act when they are transferred.

use crate::InscriptionInfo;

/// Whether an inscription can carry a BRC-20, BRC2.0 program or module
/// operation.
pub fn is_valid_brc20(inscription_info: &InscriptionInfo) -> bool {
    operation(inscription_info).is_some()
}

/// Whether the brc20 module acts on the inscription's first transfer, which
/// is when it reads the inscription's content a second time: a BRC-20
/// `transfer`, a brc20-module `withdraw`, or a BRC2.0 `deploy`, `call` or
/// `transact` (or their one-letter forms). Every other operation acts only
/// when it is inscribed.
pub fn acts_on_transfer(inscription_info: &InscriptionInfo) -> bool {
    let Some((protocol, op)) = operation(inscription_info) else {
        return false;
    };
    let ops: &[&str] = match protocol.as_str() {
        "brc-20" => &["transfer"],
        "brc20-module" => &["withdraw"],
        "brc20-prog" => &["deploy", "d", "call", "c", "transact", "t"],
        _ => &[],
    };
    op.is_some_and(|op| ops.contains(&op.as_str()))
}

/// The protocol and operation of a valid BRC-20 inscription; `None` when it
/// is not one.
fn operation(inscription_info: &InscriptionInfo) -> Option<(String, Option<String>)> {
    if inscription_info.cursed_for_brc20 || !inscription_info.is_json {
        return None;
    }
    let content_type = std::str::from_utf8(&inscription_info.content_type).ok()?;
    if !content_type.eq("text/plain")
        && !content_type.starts_with("text/plain;")
        && !content_type.eq("application/json")
        && !content_type.starts_with("application/json;")
    {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&inscription_info.content).ok()?;
    let protocol = json.get("p").and_then(|v| v.as_str())?;
    if protocol != "brc-20" && protocol != "brc20-prog" && protocol != "brc20-module" {
        return None;
    }
    if protocol == "brc20-module"
        && json.get("module").and_then(|m| m.as_str()) != Some("BRC20PROG")
    {
        return None;
    }
    let op = json.get("op").and_then(|op| op.as_str()).map(str::to_owned);
    Some((protocol.to_owned(), op))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(content: &str) -> InscriptionInfo {
        InscriptionInfo {
            inscription_id: "x".into(),
            inscription_number: 1,
            cursed_for_brc20: false,
            parent_id: None,
            is_json: true,
            content: content.as_bytes().to_vec(),
            content_type: b"text/plain;charset=utf-8".to_vec(),
            metaprotocol: Vec::new(),
        }
    }

    #[test]
    fn only_operations_that_act_on_transfer_do() {
        for (content, valid, acts) in [
            (
                r#"{"p":"brc-20","op":"transfer","tick":"ordi","amt":"1"}"#,
                true,
                true,
            ),
            (
                r#"{"p":"brc-20","op":"mint","tick":"ordi","amt":"1"}"#,
                true,
                false,
            ),
            (
                r#"{"p":"brc-20","op":"deploy","tick":"ordi","max":"1"}"#,
                true,
                false,
            ),
            (r#"{"p":"brc-20","op":"predeploy"}"#, true, false),
            (r#"{"p":"brc-20"}"#, true, false),
            (r#"{"p":"brc20-prog","op":"d","d":"0x"}"#, true, true),
            (r#"{"p":"brc20-prog","op":"transact"}"#, true, true),
            (r#"{"p":"brc20-prog","op":"mint"}"#, true, false),
            (
                r#"{"p":"brc20-module","module":"BRC20PROG","op":"withdraw"}"#,
                true,
                true,
            ),
            (
                r#"{"p":"brc20-module","module":"OTHER","op":"withdraw"}"#,
                false,
                false,
            ),
            (r#"{"p":"sns","op":"reg"}"#, false, false),
            ("not json", false, false),
        ] {
            assert_eq!(is_valid_brc20(&info(content)), valid, "{content}");
            assert_eq!(acts_on_transfer(&info(content)), acts, "{content}");
        }
        let cursed = InscriptionInfo {
            cursed_for_brc20: true,
            ..info(r#"{"p":"brc-20","op":"transfer"}"#)
        };
        assert!(!acts_on_transfer(&cursed));
    }
}
