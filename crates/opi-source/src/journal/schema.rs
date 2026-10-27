//! Byte layouts of the journal records.
//!
//! The layouts are those the OPI ord fork writes and its `db_reader` decodes
//! (`ord/` and `ord/db_reader` at commit 9a1883c): ids and satpoints use ord's
//! `Entry::store` forms, so a journal record is byte-identical to the fork's
//! record for the same event. Ids and txids
//! appear in display (hex) form in the record types and in internal byte order
//! on disk. Rune values use this journal's own length-prefixed binary layouts.

use {
    anyhow::{Context, Result, bail, ensure},
    opi_core::{
        InscriptionInfo, InscriptionRecord, RuneEtching, RuneEvent, RuneEventKind, RuneTerms,
    },
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

/// Rune id: block (`u64` BE) ++ tx index (`u32` BE).
pub const RUNE_ID_LEN: usize = 12;
/// Outpoint store form: txid ++ vout (`u32` LE).
pub const OUTPOINT_LEN: usize = TXID_LEN + 4;

/// Key of a rune event: height (`u32` BE) ++ sequence within the height
/// (`u32` BE).
pub fn rune_event_key(height: u32, seq: u32) -> [u8; 8] {
    transfer_key(height, seq)
}

/// Key of a rune etching, laid out like [`rune_event_key`].
pub fn rune_etching_key(height: u32, seq: u32) -> [u8; 8] {
    transfer_key(height, seq)
}

/// Store form of a rune id in `block:tx` form. Keys sort in numeric order.
pub fn rune_id_key(rune_id: &str) -> Result<[u8; RUNE_ID_LEN]> {
    let invalid = || format!("invalid rune id {rune_id}");
    let (block, tx) = rune_id.split_once(':').with_context(invalid)?;
    let mut out = [0; RUNE_ID_LEN];
    out[..8].copy_from_slice(&block.parse::<u64>().with_context(invalid)?.to_be_bytes());
    out[8..].copy_from_slice(&tx.parse::<u32>().with_context(invalid)?.to_be_bytes());
    Ok(out)
}

/// Inverse of [`rune_id_key`].
pub fn rune_id_string(raw: &[u8]) -> String {
    let block = u64::from_be_bytes(raw[..8].try_into().unwrap());
    let tx = u32::from_be_bytes(raw[8..RUNE_ID_LEN].try_into().unwrap());
    format!("{block}:{tx}")
}

/// `rune_counter_log` key: height (`u32` BE) ++ rune id.
pub fn rune_counter_log_key(height: u32, rune_id: &str) -> Result<[u8; 4 + RUNE_ID_LEN]> {
    let mut out = [0; 4 + RUNE_ID_LEN];
    out[..4].copy_from_slice(&height.to_be_bytes());
    out[4..].copy_from_slice(&rune_id_key(rune_id)?);
    Ok(out)
}

/// Store form of an outpoint in `txid:vout` form.
pub fn rune_outpoint_key(outpoint: &str) -> Result<[u8; OUTPOINT_LEN]> {
    let (txid, vout) = outpoint
        .split_once(':')
        .with_context(|| format!("invalid outpoint {outpoint}"))?;
    let mut out = [0; OUTPOINT_LEN];
    out[..TXID_LEN].copy_from_slice(&txid_bytes(txid)?);
    out[TXID_LEN..].copy_from_slice(
        &vout
            .parse::<u32>()
            .with_context(|| format!("invalid outpoint {outpoint}"))?
            .to_le_bytes(),
    );
    Ok(out)
}

fn outpoint_string(raw: &[u8]) -> String {
    let vout = u32::from_le_bytes(raw[TXID_LEN..OUTPOINT_LEN].try_into().unwrap());
    format!("{}:{vout}", txid_string(&raw[..TXID_LEN]))
}

/// Sequential reader over an encoded value.
struct Cursor<'a> {
    value: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Cursor<'a> {
    fn new(value: &'a [u8], what: &'static str) -> Cursor<'a> {
        Cursor {
            value,
            pos: 0,
            what,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.value.len())
            .with_context(|| format!("{} value truncated", self.what))?;
        let bytes = &self.value[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }

    fn flag(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => bail!("{} value has flag byte {other}", self.what),
        }
    }

    fn prefixed(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn opt_u64(&mut self) -> Result<Option<u64>> {
        self.flag()?.then(|| self.u64()).transpose()
    }

    fn opt_u128(&mut self) -> Result<Option<u128>> {
        self.flag()?.then(|| self.u128()).transpose()
    }

    fn finish(self) -> Result<()> {
        ensure!(
            self.pos == self.value.len(),
            "{} value has trailing bytes",
            self.what
        );
        Ok(())
    }
}

fn push_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        out.extend(value.to_be_bytes());
    }
}

fn push_opt_u128(out: &mut Vec<u8>, value: Option<u128>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        out.extend(value.to_be_bytes());
    }
}

/// Rune event value: kind (`u8`) ++ txid ++ optional outpoint ++ optional
/// length-prefixed pkscript ++ rune id ++ amount (`u128` BE). An optional is
/// a flag byte followed by the value when the flag is 1.
pub fn encode_rune_event(e: &RuneEvent) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(e.kind.id() as u8);
    out.extend(txid_bytes(&e.txid)?);
    out.push(u8::from(e.outpoint.is_some()));
    if let Some(outpoint) = &e.outpoint {
        out.extend(rune_outpoint_key(outpoint)?);
    }
    out.push(u8::from(e.pkscript.is_some()));
    if let Some(pkscript) = &e.pkscript {
        push_prefixed(&mut out, pkscript)?;
    }
    out.extend(rune_id_key(&e.rune_id)?);
    out.extend(e.amount.to_be_bytes());
    Ok(out)
}

pub fn decode_rune_event(value: &[u8]) -> Result<RuneEvent> {
    let mut c = Cursor::new(value, "rune event");
    let kind = match c.u8()? {
        0 => RuneEventKind::Input,
        1 => RuneEventKind::NewAllocation,
        2 => RuneEventKind::Mint,
        3 => RuneEventKind::Output,
        4 => RuneEventKind::Burn,
        other => bail!("rune event has kind {other}"),
    };
    let txid = txid_string(c.take(TXID_LEN)?);
    let outpoint = if c.flag()? {
        Some(outpoint_string(c.take(OUTPOINT_LEN)?))
    } else {
        None
    };
    let pkscript = if c.flag()? {
        Some(c.prefixed()?.to_vec())
    } else {
        None
    };
    let rune_id = rune_id_string(c.take(RUNE_ID_LEN)?);
    let amount = c.u128()?;
    c.finish()?;
    Ok(RuneEvent {
        kind,
        txid,
        outpoint,
        pkscript,
        rune_id,
        amount,
    })
}

/// Rune etching value: rune id ++ block (`u64` BE) ++ divisibility (`u8`) ++
/// etching txid ++ optional terms (amount, cap, height pair, offset pair, each
/// optional) ++ number (`u64` BE) ++ premine (`u128` BE) ++ length-prefixed
/// rune ++ spacers (`u32` BE) ++ optional symbol (`u32` BE scalar) ++
/// timestamp (`u64` BE) ++ turbo (`u8`).
pub fn encode_rune_etching(e: &RuneEtching) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend(rune_id_key(&e.rune_id)?);
    out.extend(e.block.to_be_bytes());
    out.push(e.divisibility);
    out.extend(txid_bytes(&e.etching)?);
    out.push(u8::from(e.terms.is_some()));
    if let Some(terms) = &e.terms {
        push_opt_u128(&mut out, terms.amount);
        push_opt_u128(&mut out, terms.cap);
        push_opt_u64(&mut out, terms.height.0);
        push_opt_u64(&mut out, terms.height.1);
        push_opt_u64(&mut out, terms.offset.0);
        push_opt_u64(&mut out, terms.offset.1);
    }
    out.extend(e.number.to_be_bytes());
    out.extend(e.premine.to_be_bytes());
    push_prefixed(&mut out, e.rune.as_bytes())?;
    out.extend(e.spacers.to_be_bytes());
    out.push(u8::from(e.symbol.is_some()));
    if let Some(symbol) = e.symbol {
        out.extend(u32::from(symbol).to_be_bytes());
    }
    out.extend(e.timestamp.to_be_bytes());
    out.push(u8::from(e.turbo));
    Ok(out)
}

pub fn decode_rune_etching(value: &[u8]) -> Result<RuneEtching> {
    let mut c = Cursor::new(value, "rune etching");
    let rune_id = rune_id_string(c.take(RUNE_ID_LEN)?);
    let block = c.u64()?;
    let divisibility = c.u8()?;
    let etching = txid_string(c.take(TXID_LEN)?);
    let terms = if c.flag()? {
        Some(RuneTerms {
            amount: c.opt_u128()?,
            cap: c.opt_u128()?,
            height: (c.opt_u64()?, c.opt_u64()?),
            offset: (c.opt_u64()?, c.opt_u64()?),
        })
    } else {
        None
    };
    let number = c.u64()?;
    let premine = c.u128()?;
    let rune = String::from_utf8(c.prefixed()?.to_vec()).context("rune name is not UTF-8")?;
    let spacers = c.u32()?;
    let symbol = if c.flag()? {
        let scalar = c.u32()?;
        Some(char::from_u32(scalar).with_context(|| format!("invalid symbol {scalar}"))?)
    } else {
        None
    };
    let timestamp = c.u64()?;
    let turbo = c.flag()?;
    c.finish()?;
    Ok(RuneEtching {
        rune_id,
        block,
        divisibility,
        etching,
        terms,
        number,
        premine,
        rune,
        spacers,
        symbol,
        timestamp,
        turbo,
    })
}

/// `rune_outpoint_balances` value: length-prefixed pkscript ++ entry count
/// (`u32` BE) ++ entries of rune id ++ amount (`u128` BE).
pub fn encode_rune_balance_row(pkscript: &[u8], balances: &[(String, u128)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    push_prefixed(&mut out, pkscript)?;
    out.extend(
        u32::try_from(balances.len())
            .context("too many balances")?
            .to_be_bytes(),
    );
    for (rune_id, amount) in balances {
        out.extend(rune_id_key(rune_id)?);
        out.extend(amount.to_be_bytes());
    }
    Ok(out)
}

/// Pkscript and `(rune id, amount)` entries of a balance row.
pub type RuneBalanceRow = (Vec<u8>, Vec<(String, u128)>);

pub fn decode_rune_balance_row(value: &[u8]) -> Result<RuneBalanceRow> {
    let mut c = Cursor::new(value, "rune balance row");
    let pkscript = c.prefixed()?.to_vec();
    let count = c.u32()?;
    let mut balances = Vec::new();
    for _ in 0..count {
        let rune_id = rune_id_string(c.take(RUNE_ID_LEN)?);
        balances.push((rune_id, c.u128()?));
    }
    c.finish()?;
    Ok((pkscript, balances))
}

/// Burned and mint totals of a rune.
pub type RuneTotals = (u128, u128);

/// `rune_counters` value: burned ++ mints (`u128` BE each).
pub fn encode_rune_counter(totals: RuneTotals) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend(totals.0.to_be_bytes());
    out.extend(totals.1.to_be_bytes());
    out
}

pub fn decode_rune_counter(value: &[u8]) -> Result<RuneTotals> {
    let mut c = Cursor::new(value, "rune counter");
    let totals = (c.u128()?, c.u128()?);
    c.finish()?;
    Ok(totals)
}

/// `rune_counter_log` value: optional totals before the block (flag byte,
/// then burned ++ mints) ++ totals after it.
pub fn encode_rune_counter_log(before: Option<RuneTotals>, after: RuneTotals) -> Vec<u8> {
    let mut out = vec![u8::from(before.is_some())];
    if let Some(before) = before {
        out.extend(encode_rune_counter(before));
    }
    out.extend(encode_rune_counter(after));
    out
}

pub fn decode_rune_counter_log(value: &[u8]) -> Result<(Option<RuneTotals>, RuneTotals)> {
    let mut c = Cursor::new(value, "rune counter log");
    let before = if c.flag()? {
        Some((c.u128()?, c.u128()?))
    } else {
        None
    };
    let after = (c.u128()?, c.u128()?);
    c.finish()?;
    Ok((before, after))
}
