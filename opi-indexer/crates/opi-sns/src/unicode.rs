//! Unicode 17.0.0 lowercasing and whitespace over the committed tables in
//! `unicode_tables.rs`, which `tools/parity/unicode_tables.py` generates.
//!
//! The functions take code points as well as `&str` because a JSON5
//! `\uXXXX` escape can produce a lone surrogate, which a Rust string cannot
//! hold.

use crate::unicode_tables::{CASE_IGNORABLE, CASED, LOWER, WHITE_SPACE};

const CAPITAL_SIGMA: u32 = 0x3A3;
const SMALL_SIGMA: u32 = 0x3C3;
const FINAL_SIGMA: u32 = 0x3C2;

/// Whether `cp` lies in one of the sorted inclusive `ranges`.
pub(crate) fn in_ranges(ranges: &[(u32, u32)], cp: u32) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The White_Space property.
pub fn is_white_space(cp: u32) -> bool {
    WHITE_SPACE.binary_search(&cp).is_ok()
}

/// The full lowercase mapping of `s`. U+03A3 becomes final sigma when a
/// cased letter precedes it and none follows, case-ignorable code points
/// skipped on both sides.
pub fn to_lowercase(s: &str) -> String {
    let cps: Vec<u32> = s.chars().map(u32::from).collect();
    lowercase_cps(&cps)
        .into_iter()
        .map(|cp| char::from_u32(cp).expect("lowercasing keeps scalar values"))
        .collect()
}

pub(crate) fn lowercase_cps(cps: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(cps.len());
    for (i, &cp) in cps.iter().enumerate() {
        if cp == CAPITAL_SIGMA {
            out.push(if final_sigma(cps, i) {
                FINAL_SIGMA
            } else {
                SMALL_SIGMA
            });
            continue;
        }
        match LOWER.binary_search_by_key(&cp, |&(from, _)| from) {
            Ok(at) => out.extend_from_slice(LOWER[at].1),
            Err(_) => out.push(cp),
        }
    }
    out
}

/// The Final_Sigma condition at `cps[i]`.
fn final_sigma(cps: &[u32], i: usize) -> bool {
    let ignorable = |cp: u32| in_ranges(CASE_IGNORABLE, cp);
    let cased = |cp: u32| in_ranges(CASED, cp);
    let before = cps[..i].iter().rev().find(|&&cp| !ignorable(cp));
    if !before.is_some_and(|&cp| cased(cp)) {
        return false;
    }
    match cps[i + 1..].iter().find(|&&cp| !ignorable(cp)) {
        None => true,
        Some(&cp) => !cased(cp),
    }
}

/// `cps` cut at its first whitespace and trimmed of whitespace: everything
/// before the first whitespace code point, which is empty when `cps` starts
/// with whitespace and leaves nothing for the trim to remove.
pub(crate) fn cut_and_trim(cps: &[u32]) -> &[u32] {
    let end = cps
        .iter()
        .position(|&cp| is_white_space(cp))
        .unwrap_or(cps.len());
    &cps[..end]
}
