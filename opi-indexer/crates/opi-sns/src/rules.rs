//! The SNS rules of `modules/sns_index/sns_index.py` (`get_sns_name`,
//! `get_ns_register` and `index_block`) and of db_reader's `is_valid_sns`.

use opi_core::{BlockView, InscriptionInfo};
use sha2::{Digest, Sha256};

use crate::json5::{self, PyValue};
use crate::pystr::{from_cps, lower_cps, split_first_cps, strip_cps};

pub const EVENT_SEPARATOR: &str = "|";
const TEXT_PLAIN: &[u8] = b"text/plain";
const APPLICATION_JSON: &[u8] = b"application/json";
const MAX_UTF8_LEN: usize = 2048;
const DOT: u32 = '.' as u32;
const NUL: u32 = 0;

/// A registration an inscription makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnsOp {
    Name { name: String, domain: String },
    Namespace(String),
}

/// Content whose name or namespace holds a lone surrogate. `sns_index.py`
/// raises on it (`utf8len` encodes it) and retries the block forever, so the
/// block cannot be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unencodable;

/// `get_sns_name`: the name and domain as code points.
///
/// The name is the content itself unless `json5.loads` returns a value that
/// decides otherwise: a dict must carry `p == "sns"`, `op == "reg"` and a
/// string `name`; a string or list must contain `"p"`, `"op"` and `"name"`
/// (as substrings or members), after which subscripting it raises and the
/// content stays the name; any other value raises on `in` and keeps the
/// content too.
pub fn get_sns_name(content: &[u8]) -> Option<(Vec<u32>, Vec<u32>)> {
    let content = std::str::from_utf8(content).ok()?;
    let name = match json5::loads(content) {
        Some(value @ PyValue::Dict(_)) => match keyed(&value, "reg", "name")? {
            PyValue::Str(name) => name.clone(),
            _ => return None,
        },
        Some(value @ (PyValue::Str(_) | PyValue::List(_))) => {
            if !["p", "op", "name"].iter().all(|key| contains(&value, key)) {
                return None;
            }
            crate::pystr::to_cps(content)
        }
        _ => crate::pystr::to_cps(content),
    };
    let name = first_token(&name)?;
    if name.iter().filter(|&&cp| cp == DOT).count() != 1 {
        return None;
    }
    let dot = name.iter().position(|&cp| cp == DOT)?;
    let domain = name[dot + 1..].to_vec();
    Some((name, domain))
}

/// `get_ns_register`: the namespace as code points. Only a dict with
/// `p == "sns"`, `op == "ns"` and a string `ns` registers one.
pub fn get_ns_register(content: &[u8]) -> Option<Vec<u32>> {
    let content = std::str::from_utf8(content).ok()?;
    let value = json5::loads(content)?;
    let PyValue::Str(namespace) = keyed(&value, "ns", "ns")? else {
        return None;
    };
    let namespace = first_token(namespace)?;
    if namespace.contains(&DOT) {
        return None;
    }
    Some(namespace)
}

/// The value under `key` of a dict that has `p`, `op` and `key`, with
/// `p == "sns"` and `op == op_value`.
fn keyed<'v>(value: &'v PyValue, op_value: &str, key: &str) -> Option<&'v PyValue> {
    let p = value.get("p")?;
    let op = value.get("op")?;
    let found = value.get(key)?;
    (p.is_str("sns") && op.is_str(op_value)).then_some(found)
}

/// Python `key in value` for a str (substring) or list (membership).
fn contains(value: &PyValue, key: &str) -> bool {
    let key = crate::pystr::to_cps(key);
    match value {
        PyValue::Str(s) => s.windows(key.len()).any(|window| window == key.as_slice()),
        PyValue::List(items) => items
            .iter()
            .any(|item| matches!(item, PyValue::Str(s) if *s == key)),
        _ => false,
    }
}

/// `s.lower().split()[0].strip()`.
fn first_token(s: &[u32]) -> Option<Vec<u32>> {
    let lower = lower_cps(s);
    let token = split_first_cps(&lower)?;
    Some(strip_cps(token).to_vec())
}

/// What `index_block` does with valid content: the name when there is one,
/// else the namespace, each dropped when it holds `\0` or encodes to more
/// than 2048 UTF-8 bytes.
pub fn parse(content: &[u8]) -> Result<Option<SnsOp>, Unencodable> {
    if let Some((name, domain)) = get_sns_name(content) {
        if name.contains(&NUL) || domain.contains(&NUL) {
            return Ok(None);
        }
        let name = encodable(&name)?;
        if name.len() > MAX_UTF8_LEN {
            return Ok(None);
        }
        let domain = from_cps(&domain);
        return Ok(Some(SnsOp::Name { name, domain }));
    }
    if let Some(namespace) = get_ns_register(content) {
        if namespace.contains(&NUL) {
            return Ok(None);
        }
        let namespace = encodable(&namespace)?;
        if namespace.len() > MAX_UTF8_LEN {
            return Ok(None);
        }
        return Ok(Some(SnsOp::Namespace(namespace)));
    }
    Ok(None)
}

fn encodable(cps: &[u32]) -> Result<String, Unencodable> {
    if cps.iter().any(|&cp| char::from_u32(cp).is_none()) {
        return Err(Unencodable);
    }
    Ok(from_cps(cps))
}

/// db_reader's prefilter: non-cursed, with a content type starting
/// `text/plain` or `application/json`.
pub fn is_valid_sns(info: &InscriptionInfo) -> bool {
    info.inscription_number >= 0
        && (info.content_type.starts_with(TEXT_PLAIN)
            || info.content_type.starts_with(APPLICATION_JSON))
}

/// `index_block`'s content type check: exactly `text/plain` or
/// `application/json`, or either followed by `;`.
pub fn content_type_accepted(content_type: &[u8]) -> bool {
    [TEXT_PLAIN, APPLICATION_JSON].iter().any(|base| {
        content_type
            .strip_prefix(*base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(b";"))
    })
}

pub fn register_event_str(name: &str, domain: &str, inscription_id: &str) -> String {
    format!("register;{inscription_id};{name};{domain}")
}

pub fn ns_register_event_str(namespace: &str, inscription_id: &str) -> String {
    format!("ns_register;{inscription_id};{namespace}")
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `sha256` of the events joined with `|`; an empty block hashes `""`.
pub fn block_event_hash(events: &[String]) -> String {
    sha256_hex(&events.join(EVENT_SEPARATOR))
}

/// Chains the block hash onto the previous cumulative hash, which is absent
/// at the first indexed height.
pub fn cumulative_hash(previous: Option<&str>, block_event_hash: &str) -> String {
    match previous {
        None => block_event_hash.to_string(),
        Some(previous) => sha256_hex(&format!("{previous}{block_event_hash}")),
    }
}

/// An SNS inscription of a block that may become a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub inscription_id: String,
    pub inscription_number: i32,
    pub op: SnsOp,
}

/// SNS registrations of `block` in ascending inscription number: creations
/// that pass `is_valid_sns` and the content type check and parse to a name
/// or namespace. A creation without an inscription info, or one the Python
/// indexer cannot encode, is an error naming the inscription.
pub fn candidates(block: &BlockView) -> anyhow::Result<Vec<Candidate>> {
    let feed = &block.inscriptions;
    let mut valid: Vec<&InscriptionInfo> = Vec::new();
    for record in feed.records.iter().filter(|r| r.old_satpoint.is_none()) {
        let info = feed.infos.get(&record.inscription_id).ok_or_else(|| {
            anyhow::anyhow!(
                "block {} has no inscription info for {}",
                block.height,
                record.inscription_id
            )
        })?;
        if is_valid_sns(info) {
            valid.push(info);
        }
    }
    valid.sort_by_key(|info| info.inscription_number);
    let mut found = Vec::new();
    for info in valid {
        if !content_type_accepted(&info.content_type) {
            continue;
        }
        let op = parse(&info.content).map_err(|Unencodable| {
            anyhow::anyhow!(
                "inscription {} at block {} registers an SNS name with a lone \
                 surrogate, which sns_index.py cannot encode",
                info.inscription_id,
                block.height
            )
        })?;
        if let Some(op) = op {
            found.push(Candidate {
                inscription_id: info.inscription_id.clone(),
                inscription_number: info.inscription_number,
                op,
            });
        }
    }
    Ok(found)
}
