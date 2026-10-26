//! The SNS indexing rules: the written spec
//! (<https://docs.satsnames.org/sats-names/sns-spec/index-names>,
//! <https://docs.satsnames.org/sats-names/sns-spec/mint-names>) with the
//! decisions it leaves open. `opi-indexer/README.md` ("SNS indexing rules")
//! lists the same rules.
//!
//! 1. Inscriptions: creations of blessed inscriptions (number >= 0) whose
//!    content type is `text/plain` or `application/json`, alone or followed
//!    by `;` parameters, in ascending inscription number.
//! 2. Content that is not valid UTF-8 registers nothing.
//! 3. Names: if the text parses as JSON5 ([`crate::json5`]), it must be an
//!    object with `p == "sns"`, `op == "reg"` and a string `name`, which is
//!    the candidate; any other parsed value registers nothing. If it does not
//!    parse, the whole text is the candidate. The candidate is lowercased
//!    (full mapping with final sigma) and reduced to its first
//!    whitespace-separated token (leading whitespace trimmed, then cut at the
//!    next whitespace). It must
//!    hold exactly one `.` with text on both sides; the namespace (stored as
//!    `domain`) is the part after it.
//! 4. Namespaces: only a JSON5 object with `p == "sns"`, `op == "ns"` and a
//!    string `ns`. It is lowercased and must be non-empty, without `.` and
//!    without whitespace. When an inscription yields both a name and a
//!    namespace, the name wins.
//! 5. Whitespace is the Unicode White_Space property.
//! 6. Case mapping and White_Space come from committed Unicode 17.0.0
//!    tables.
//! 7. JSON5 nested deeper than 128 arrays or objects does not parse.
//! 8. A name or namespace holding a lone surrogate (from a `\u` escape) is
//!    skipped.
//! 9. A name or namespace holding `\0`, or longer than 2048 UTF-8 bytes, is
//!    skipped.
//! 10. The first inscription of a name or namespace wins; later ones emit no
//!     event.

use opi_core::{BlockView, InscriptionInfo};
use sha2::{Digest, Sha256};

use crate::json5::{self, Value};
use crate::unicode::{first_token, is_white_space, lowercase_cps};

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

/// The registration `content` makes, if any (rules 2-9).
pub fn parse(content: &[u8]) -> Option<SnsOp> {
    let text = std::str::from_utf8(content).ok()?;
    let value = json5::parse(text);
    name(text, value.as_ref()).or_else(|| namespace(value.as_ref()))
}

/// Rule 3: the name and its namespace.
fn name(text: &str, value: Option<&Value>) -> Option<SnsOp> {
    let candidate = match value {
        Some(value) => match sns_field(value, "reg", "name")? {
            Value::Str(name) => lowercase_cps(name),
            _ => return None,
        },
        None => lowercase_cps(&text.chars().map(u32::from).collect::<Vec<_>>()),
    };
    let name = first_token(&candidate);
    if name.iter().filter(|&&cp| cp == DOT).count() != 1 {
        return None;
    }
    let dot = name.iter().position(|&cp| cp == DOT)?;
    if dot == 0 || dot == name.len() - 1 {
        return None;
    }
    let name = storable(name)?;
    let domain = name[name.find('.')? + 1..].to_string();
    Some(SnsOp::Name { name, domain })
}

/// Rule 4: the namespace.
fn namespace(value: Option<&Value>) -> Option<SnsOp> {
    let Value::Str(namespace) = sns_field(value?, "ns", "ns")? else {
        return None;
    };
    let namespace = lowercase_cps(namespace);
    if namespace.is_empty()
        || namespace.contains(&DOT)
        || namespace.iter().any(|&cp| is_white_space(cp))
    {
        return None;
    }
    storable(&namespace).map(SnsOp::Namespace)
}

/// The field `key` of an object with `p == "sns"` and `op == op`.
fn sns_field<'v>(value: &'v Value, op: &str, key: &str) -> Option<&'v Value> {
    let is_sns = value.get("p")?.is_str("sns") && value.get("op")?.is_str(op);
    is_sns.then(|| value.get(key)).flatten()
}

/// Rules 8 and 9: the text of `cps` when it has no lone surrogate, no NUL
/// and at most 2048 UTF-8 bytes.
fn storable(cps: &[u32]) -> Option<String> {
    if cps.contains(&NUL) {
        return None;
    }
    let text: String = cps
        .iter()
        .map(|&cp| char::from_u32(cp))
        .collect::<Option<_>>()?;
    (text.len() <= MAX_UTF8_LEN).then_some(text)
}

/// Rule 1's prefilter: blessed, with a content type starting `text/plain` or
/// `application/json`.
pub fn is_valid_sns(info: &InscriptionInfo) -> bool {
    info.inscription_number >= 0
        && (info.content_type.starts_with(TEXT_PLAIN)
            || info.content_type.starts_with(APPLICATION_JSON))
}

/// Rule 1's content type: exactly `text/plain` or `application/json`, or
/// either followed by `;`.
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

/// SNS registrations of `block` in ascending inscription number (rule 1). A
/// creation without an inscription info is an error naming the inscription.
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
        if is_valid_sns(info) && content_type_accepted(&info.content_type) {
            valid.push(info);
        }
    }
    valid.sort_by_key(|info| info.inscription_number);
    Ok(valid
        .into_iter()
        .filter_map(|info| {
            parse(&info.content).map(|op| Candidate {
                inscription_id: info.inscription_id.clone(),
                inscription_number: info.inscription_number,
                op,
            })
        })
        .collect())
}
