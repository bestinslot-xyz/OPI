use bitcoin::{Address, Network, Script};

/// The address of `pkscript` on `network`, encoded as the brc20 module does:
/// mainnet encoding on mainnet, testnet encoding on testnet, testnet4 and
/// signet. `None` for a script that is not P2PKH, P2SH, P2WPKH, P2WSH or
/// P2TR, or an unknown network.
pub fn address(pkscript: &[u8], network: &str) -> Option<String> {
    let network = match network {
        "mainnet" => Network::Bitcoin,
        "testnet" | "testnet4" | "signet" => Network::Testnet,
        "regtest" => Network::Regtest,
        _ => return None,
    };
    let script = Script::from_bytes(pkscript);
    let known = script.is_p2pkh()
        || script.is_p2sh()
        || script.is_p2wpkh()
        || script.is_p2wsh()
        || script.is_p2tr();
    if !known {
        return None;
    }
    Address::from_script(script, network)
        .ok()
        .map(|address| address.to_string())
}

/// A content type as stored: lossy UTF-8 without NUL bytes, which a
/// Postgres text value cannot hold.
pub fn content_type_text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).replace('\0', "")
}

/// The media type a content type is filtered by: lowercased, parameters
/// after `;` split off, surrounding whitespace trimmed. `None` when nothing
/// is left.
pub fn media_type(content_type: &str) -> Option<String> {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (!media.is_empty()).then_some(media)
}

/// Splits a `txid:vout:offset` satpoint into its `txid:vout` outpoint and
/// its offset.
pub fn split_satpoint(satpoint: &str) -> anyhow::Result<(String, u64)> {
    let (outpoint, offset) = satpoint
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid satpoint {satpoint}"))?;
    let offset = offset
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid satpoint {satpoint}"))?;
    Ok((outpoint.to_owned(), offset))
}

/// Id of an event row: its height in the high 32 bits and its position
/// among the height's moves in the low 32, so ids ascend in event order
/// whatever rollbacks and pruning removed.
pub fn event_id(height: u32, position: usize) -> anyhow::Result<i64> {
    let position = u32::try_from(position)?;
    Ok((i64::from(height) << 32) | i64::from(position))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_is_lowercased_without_parameters() {
        assert_eq!(
            media_type("Text/Plain;charset=utf-8").as_deref(),
            Some("text/plain")
        );
        assert_eq!(media_type(" image/PNG ").as_deref(), Some("image/png"));
        assert_eq!(media_type(";charset=utf-8"), None);
        assert_eq!(media_type(""), None);
    }

    #[test]
    fn content_type_text_drops_nul_and_invalid_utf8() {
        assert_eq!(content_type_text(b"text/plain\0"), "text/plain");
        assert_eq!(content_type_text(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn satpoint_splits_at_the_last_colon() {
        let txid = "ab".repeat(32);
        assert_eq!(
            split_satpoint(&format!("{txid}:1:42")).unwrap(),
            (format!("{txid}:1"), 42)
        );
        assert!(split_satpoint("nope").is_err());
    }

    #[test]
    fn event_ids_ascend_by_height_then_position() {
        assert!(event_id(10, u32::MAX as usize).unwrap() < event_id(11, 0).unwrap());
        assert!(event_id(10, 0).unwrap() < event_id(10, 1).unwrap());
        assert!(event_id(1, u32::MAX as usize + 1).is_err());
    }

    #[test]
    fn address_follows_the_network() {
        let p2wpkh = hex::decode(format!("0014{}", "11".repeat(20))).unwrap();
        assert!(address(&p2wpkh, "mainnet").unwrap().starts_with("bc1q"));
        assert!(address(&p2wpkh, "signet").unwrap().starts_with("tb1q"));
        assert_eq!(address(&[0x6a, 0x01, 0x00], "mainnet"), None);
        assert_eq!(address(&[], "mainnet"), None);
    }
}
