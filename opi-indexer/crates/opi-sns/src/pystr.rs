//! Python `str` methods the SNS rules call, over the tables in `pytables.rs`
//! that `tools/parity/pystr_tables.py` generates from the reference Python.
//!
//! Each method has a code point form for strings that may hold lone
//! surrogates (json5 `\uXXXX` escapes produce them) and a `&str` form.

use crate::pytables::{CASE_IGNORABLE, CASED, LOWER, PY_SPACE};

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

/// `chr(cp).isspace()`.
pub fn is_py_space(cp: u32) -> bool {
    PY_SPACE.binary_search(&cp).is_ok()
}

/// Python's `str.lower()`: the full lowercase mapping of each code point,
/// with U+03A3 becoming final sigma when a cased letter precedes it and none
/// follows, case-ignorable code points skipped on both sides.
pub fn py_lower(s: &str) -> String {
    from_cps(&lower_cps(&to_cps(s)))
}

pub(crate) fn lower_cps(cps: &[u32]) -> Vec<u32> {
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

/// `handle_capital_sigma` of CPython's Objects/unicodeobject.c.
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

/// `s.split()[0]`, or `None` when `s.split()` is empty.
pub fn py_split_first(s: &str) -> Option<&str> {
    let start = s.find(|c: char| !is_py_space(u32::from(c)))?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| is_py_space(u32::from(c)))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

pub(crate) fn split_first_cps(cps: &[u32]) -> Option<&[u32]> {
    let start = cps.iter().position(|&cp| !is_py_space(cp))?;
    let rest = &cps[start..];
    let end = rest
        .iter()
        .position(|&cp| is_py_space(cp))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// `s.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| is_py_space(u32::from(c)))
}

pub(crate) fn strip_cps(cps: &[u32]) -> &[u32] {
    let start = cps
        .iter()
        .position(|&cp| !is_py_space(cp))
        .unwrap_or(cps.len());
    let end = cps
        .iter()
        .rposition(|&cp| !is_py_space(cp))
        .map_or(start, |at| at + 1);
    &cps[start..end]
}

pub(crate) fn to_cps(s: &str) -> Vec<u32> {
    s.chars().map(u32::from).collect()
}

/// The string of `cps`, which hold no surrogates.
pub(crate) fn from_cps(cps: &[u32]) -> String {
    cps.iter()
        .map(|&cp| char::from_u32(cp).expect("a code point outside the surrogates"))
        .collect()
}
