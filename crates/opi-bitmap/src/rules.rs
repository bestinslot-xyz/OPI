use std::collections::HashSet;

use opi_core::{BlockView, InscriptionInfo};
use sha2::{Digest, Sha256};

pub const EVENT_SEPARATOR: &str = "|";
const BITMAP_SUFFIX: &str = ".bitmap";
const TEXT_PLAIN: &[u8] = b"text/plain";

/// Parses `<digits>.bitmap` content into its number. The digits are ASCII
/// only and carry no leading zero unless the number is exactly `0`. Numbers
/// beyond `u64` yield `None`; they exceed any block height.
pub fn bitmap_number(content: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(content).ok()?;
    let digits = text.strip_suffix(BITMAP_SUFFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.starts_with('0') && digits.len() != 1 {
        return None;
    }
    digits.parse().ok()
}

/// Inscriptions the bitmap indexer reads: non-cursed, not JSON, `text/plain`.
pub fn is_valid_bitmap(info: &InscriptionInfo) -> bool {
    info.inscription_number >= 0 && !info.is_json && info.content_type.starts_with(TEXT_PLAIN)
}

pub fn event_str(bitmap_number: u64, inscription_id: &str) -> String {
    format!("inscribe;{inscription_id};{bitmap_number}")
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

/// A bitmap inscription of a block that may become a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub inscription_id: String,
    pub inscription_number: i32,
    pub bitmap_number: u64,
}

/// Valid bitmap creations of `block` in ascending inscription number, without
/// numbers above the block height and keeping only the first inscription of a
/// number within the block. A creation without an inscription info is an error
/// naming the inscription.
pub fn candidates(block: &BlockView) -> anyhow::Result<Vec<Candidate>> {
    let feed = &block.inscriptions;
    let mut found: Vec<(&InscriptionInfo, u64)> = Vec::new();
    for record in feed.records.iter().filter(|r| r.old_satpoint.is_none()) {
        let info = feed.infos.get(&record.inscription_id).ok_or_else(|| {
            anyhow::anyhow!(
                "block {} has no inscription info for {}",
                block.height,
                record.inscription_id
            )
        })?;
        if !is_valid_bitmap(info) {
            continue;
        }
        if let Some(number) = bitmap_number(&info.content)
            && number <= u64::from(block.height)
        {
            found.push((info, number));
        }
    }
    found.sort_by_key(|(info, _)| info.inscription_number);
    let mut seen = HashSet::new();
    Ok(found
        .into_iter()
        .filter(|(_, number)| seen.insert(*number))
        .map(|(info, number)| Candidate {
            inscription_id: info.inscription_id.clone(),
            inscription_number: info.inscription_number,
            bitmap_number: number,
        })
        .collect())
}
