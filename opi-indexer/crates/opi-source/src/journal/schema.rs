//! Byte layouts of the journal records.
//!
//! The layouts are those the OPI ord fork writes and its `db_reader` decodes:
//! ids and satpoints use ord's `Entry::store` forms, so a journal record is
//! byte-identical to the fork's record for the same event. Ids and txids
//! appear in display (hex) form in the record types and in internal byte order
//! on disk.

use {
    anyhow::{Context, Result, bail, ensure},
    opi_core::{InscriptionInfo, InscriptionRecord},
};

/// Bytes of a txid.
pub const TXID_LEN: usize = 32;
/// Inscription id: txid ++ index (`u32` BE).
pub const INSCRIPTION_ID_LEN: usize = TXID_LEN + 4;
/// Satpoint: txid ++ vout (`u32` LE) ++ offset (`u64` LE).
pub const SATPOINT_LEN: usize = TXID_LEN + 4 + 8;
/// Vout of the null outpoint.
pub const NULL_VOUT: u32 = u32::MAX;
/// Transfer key: height (`u32` BE) ++ idx (`u32` BE).
pub const TRANSFER_KEY_LEN: usize = 8;

/// Transfer value field offsets.
pub const TRANSFER_OLD_SATPOINT: usize = INSCRIPTION_ID_LEN;
pub const TRANSFER_NEW_SATPOINT: usize = TRANSFER_OLD_SATPOINT + SATPOINT_LEN;
pub const TRANSFER_SENT_AS_FEE: usize = TRANSFER_NEW_SATPOINT + SATPOINT_LEN;
pub const TRANSFER_OUTPUT_VALUE: usize = TRANSFER_SENT_AS_FEE + 1;
pub const TRANSFER_TXID: usize = TRANSFER_OUTPUT_VALUE + 8;
pub const TRANSFER_PKSCRIPT: usize = TRANSFER_TXID + TXID_LEN;

/// Info value field offsets up to the first length prefix.
pub const INFO_CURSED: usize = 4;
pub const INFO_PARENT: usize = INFO_CURSED + 1;
pub const INFO_IS_JSON: usize = INFO_PARENT + INSCRIPTION_ID_LEN;
pub const INFO_CONTENT_LEN: usize = INFO_IS_JSON + 1;

/// Inscription id string standing for "no parent".
pub const NO_PARENT_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000i0";

pub fn transfer_key(height: u32, idx: u32) -> [u8; TRANSFER_KEY_LEN] {
    let mut key = [0; TRANSFER_KEY_LEN];
    key[..4].copy_from_slice(&height.to_be_bytes());
    key[4..].copy_from_slice(&idx.to_be_bytes());
    key
}

/// Raw event key: height (`u32` BE) ++ sequence within the height (`u32` BE).
pub fn raw_event_key(height: u32, seq: u32) -> [u8; 8] {
    transfer_key(height, seq)
}

fn txid_bytes(txid: &str) -> Result<[u8; TXID_LEN]> {
    let mut bytes: [u8; TXID_LEN] = hex::decode(txid)
        .with_context(|| format!("invalid txid {txid}"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("txid {txid} is not {TXID_LEN} bytes"))?;
    bytes.reverse();
    Ok(bytes)
}

fn txid_string(raw: &[u8]) -> String {
    let mut bytes = raw.to_vec();
    bytes.reverse();
    hex::encode(bytes)
}

fn encode_inscription_id(id: &str) -> Result<[u8; INSCRIPTION_ID_LEN]> {
    let (txid, index) = id
        .split_once('i')
        .with_context(|| format!("invalid inscription id {id}"))?;
    let mut out = [0; INSCRIPTION_ID_LEN];
    out[..TXID_LEN].copy_from_slice(&txid_bytes(txid)?);
    out[TXID_LEN..].copy_from_slice(
        &index
            .parse::<u32>()
            .with_context(|| format!("invalid inscription id {id}"))?
            .to_be_bytes(),
    );
    Ok(out)
}

/// `ord_inscription_info` key of an inscription id in `txidi<index>` form.
pub fn info_key(inscription_id: &str) -> Result<[u8; INSCRIPTION_ID_LEN]> {
    encode_inscription_id(inscription_id)
}

fn decode_inscription_id(raw: &[u8]) -> String {
    let index = u32::from_be_bytes(raw[TXID_LEN..INSCRIPTION_ID_LEN].try_into().unwrap());
    format!("{}i{index}", txid_string(&raw[..TXID_LEN]))
}

fn encode_satpoint(satpoint: &str) -> Result<[u8; SATPOINT_LEN]> {
    let mut parts = satpoint.split(':');
    let (Some(txid), Some(vout), Some(offset), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("invalid satpoint {satpoint}");
    };
    let mut out = [0; SATPOINT_LEN];
    out[..TXID_LEN].copy_from_slice(&txid_bytes(txid)?);
    out[TXID_LEN..TXID_LEN + 4].copy_from_slice(
        &vout
            .parse::<u32>()
            .with_context(|| format!("invalid satpoint {satpoint}"))?
            .to_le_bytes(),
    );
    out[TXID_LEN + 4..].copy_from_slice(
        &offset
            .parse::<u64>()
            .with_context(|| format!("invalid satpoint {satpoint}"))?
            .to_le_bytes(),
    );
    Ok(out)
}

fn decode_satpoint(raw: &[u8]) -> String {
    let vout = u32::from_le_bytes(raw[TXID_LEN..TXID_LEN + 4].try_into().unwrap());
    let offset = u64::from_le_bytes(raw[TXID_LEN + 4..SATPOINT_LEN].try_into().unwrap());
    format!("{}:{vout}:{offset}", txid_string(&raw[..TXID_LEN]))
}

/// The satpoint stored for a creation: the null outpoint at offset 0.
fn null_satpoint() -> [u8; SATPOINT_LEN] {
    let mut out = [0; SATPOINT_LEN];
    out[TXID_LEN..TXID_LEN + 4].copy_from_slice(&NULL_VOUT.to_le_bytes());
    out
}

pub fn encode_transfer(r: &InscriptionRecord) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(TRANSFER_PKSCRIPT + r.new_pkscript.len());
    out.extend(encode_inscription_id(&r.inscription_id)?);
    match &r.old_satpoint {
        Some(old) => out.extend(encode_satpoint(old)?),
        None => out.extend(null_satpoint()),
    }
    out.extend(encode_satpoint(&r.new_satpoint)?);
    out.push(u8::from(r.sent_as_fee));
    out.extend(r.new_output_value.to_be_bytes());
    out.extend(txid_bytes(&r.txid)?);
    out.extend(&r.new_pkscript);
    Ok(out)
}

pub fn decode_transfer(key: &[u8], value: &[u8]) -> Result<InscriptionRecord> {
    ensure!(
        key.len() == TRANSFER_KEY_LEN,
        "transfer key is {} bytes, expected {TRANSFER_KEY_LEN}",
        key.len()
    );
    ensure!(
        value.len() >= TRANSFER_PKSCRIPT,
        "transfer value is {} bytes, expected at least {TRANSFER_PKSCRIPT}",
        value.len()
    );
    let old = &value[TRANSFER_OLD_SATPOINT..TRANSFER_NEW_SATPOINT];
    let old_satpoint = (old[..TXID_LEN] != [0; TXID_LEN]).then(|| decode_satpoint(old));
    Ok(InscriptionRecord {
        height: u32::from_be_bytes(key[..4].try_into().unwrap()),
        idx: u32::from_be_bytes(key[4..].try_into().unwrap()),
        inscription_id: decode_inscription_id(&value[..INSCRIPTION_ID_LEN]),
        old_satpoint,
        new_satpoint: decode_satpoint(&value[TRANSFER_NEW_SATPOINT..TRANSFER_SENT_AS_FEE]),
        sent_as_fee: value[TRANSFER_SENT_AS_FEE] != 0,
        new_output_value: u64::from_be_bytes(
            value[TRANSFER_OUTPUT_VALUE..TRANSFER_TXID]
                .try_into()
                .unwrap(),
        ),
        txid: txid_string(&value[TRANSFER_TXID..TRANSFER_PKSCRIPT]),
        new_pkscript: value[TRANSFER_PKSCRIPT..].to_vec(),
    })
}

fn push_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    out.extend(
        u32::try_from(bytes.len())
            .context("field exceeds u32 length")?
            .to_be_bytes(),
    );
    out.extend(bytes);
    Ok(())
}

pub fn encode_info(i: &InscriptionInfo) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend(i.inscription_number.to_be_bytes());
    out.push(u8::from(i.cursed_for_brc20));
    out.extend(encode_inscription_id(
        i.parent_id.as_deref().unwrap_or(NO_PARENT_ID),
    )?);
    out.push(u8::from(i.is_json));
    push_prefixed(&mut out, &i.content)?;
    push_prefixed(&mut out, &i.content_type)?;
    push_prefixed(&mut out, &i.metaprotocol)?;
    Ok(out)
}

fn take_prefixed<'a>(value: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let len_end = *pos + 4;
    ensure!(
        value.len() >= len_end,
        "info value truncated at length prefix"
    );
    let len = u32::from_be_bytes(value[*pos..len_end].try_into().unwrap()) as usize;
    let end = len_end
        .checked_add(len)
        .filter(|end| *end <= value.len())
        .context("info value truncated in field")?;
    *pos = end;
    Ok(&value[len_end..end])
}

pub fn decode_info(id: &str, value: &[u8]) -> Result<InscriptionInfo> {
    ensure!(
        value.len() >= INFO_CONTENT_LEN,
        "info value is {} bytes, expected at least {INFO_CONTENT_LEN}",
        value.len()
    );
    let parent = &value[INFO_PARENT..INFO_IS_JSON];
    let parent_id = (parent[..TXID_LEN] != [0; TXID_LEN]).then(|| decode_inscription_id(parent));
    let mut pos = INFO_CONTENT_LEN;
    let content = take_prefixed(value, &mut pos)?.to_vec();
    let content_type = take_prefixed(value, &mut pos)?.to_vec();
    let metaprotocol = take_prefixed(value, &mut pos)?.to_vec();
    ensure!(pos == value.len(), "info value has trailing bytes");
    Ok(InscriptionInfo {
        inscription_id: id.to_owned(),
        inscription_number: i32::from_be_bytes(value[..INFO_CURSED].try_into().unwrap()),
        cursed_for_brc20: value[INFO_CURSED] != 0,
        parent_id,
        is_json: value[INFO_IS_JSON] != 0,
        content,
        content_type,
        metaprotocol,
    })
}
