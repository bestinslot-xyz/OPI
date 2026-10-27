//! The parameter rules the routes without a JavaScript original share: a
//! refused parameter's 400 answer, single-valued and integer parameters,
//! the page limits and the LIKE prefix pattern.

use crate::{
    js::{JsObject, JsValue},
    reply::Reply,
};

/// The page size when a request names none.
pub const DEFAULT_LIMIT: i64 = 100;

/// The largest page a request may ask for.
pub const MAX_LIMIT: i64 = 500;

/// The largest offset a request may ask for.
pub const MAX_OFFSET: i64 = 1_000_000_000;

/// The longest `inscription_id` a request may name.
pub const MAX_INSCRIPTION_ID_LEN: usize = 128;

/// The longest `address` a request may name.
pub const MAX_ADDRESS_LEN: usize = 128;

/// The longest ticker a request may name, in bytes.
pub const MAX_TICKER_LEN: usize = 256;

/// The page of `rows`, fetched as one more than `limit`, and whether more
/// follow.
pub fn split_page<T>(mut rows: Vec<T>, limit: i64) -> (Vec<T>, bool) {
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    (rows, has_more)
}

/// A parameter the route refuses, with the reason it answers in its 400.
#[derive(Debug, PartialEq, Eq)]
pub struct BadParam(pub String);

impl BadParam {
    pub(crate) fn reply(self) -> Reply {
        Reply::error(400, &self.0)
    }
}

/// The string value of `key`, or `None` when the request leaves it out. A
/// repeated or bracketed parameter is refused.
pub fn single<'a>(query: &'a JsObject, key: &str) -> Result<Option<&'a str>, BadParam> {
    match query.get(key) {
        JsValue::Undefined => Ok(None),
        JsValue::String(value) => Ok(Some(value)),
        _ => Err(BadParam(format!("{key} must be a single value"))),
    }
}

/// The non-empty string value of `key`.
pub fn required<'a>(query: &'a JsObject, key: &str) -> Result<&'a str, BadParam> {
    match single(query, key)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(BadParam(format!("{key} is required"))),
    }
}

/// The integer value of `key` in `min..=max`, written in decimal digits, or
/// `default` when the request leaves it out.
pub fn integer(
    query: &JsObject,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, BadParam> {
    let Some(text) = single(query, key)? else {
        return Ok(default);
    };
    let invalid = || BadParam(format!("{key} must be an integer from {min} to {max}"));
    if text.is_empty() || text.len() > 18 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let value: i64 = text.parse().map_err(|_| invalid())?;
    if !(min..=max).contains(&value) {
        return Err(invalid());
    }
    Ok(value)
}

/// The non-empty string value of `key`, at most `max_len` bytes and without
/// NUL (Postgres text holds none).
pub fn text<'a>(query: &'a JsObject, key: &str, max_len: usize) -> Result<&'a str, BadParam> {
    let value = required(query, key)?;
    check_text(key, value, max_len)?;
    Ok(value)
}

/// The string value of `key` under the rules of [`text`], or `None` when
/// the request leaves it out.
pub fn optional_text<'a>(
    query: &'a JsObject,
    key: &str,
    max_len: usize,
) -> Result<Option<&'a str>, BadParam> {
    let Some(value) = single(query, key)? else {
        return Ok(None);
    };
    check_text(key, value, max_len)?;
    Ok(Some(value))
}

fn check_text(key: &str, value: &str, max_len: usize) -> Result<(), BadParam> {
    if value.len() > max_len || value.contains('\0') {
        return Err(BadParam(format!(
            "{key} must be at most {max_len} bytes without NUL"
        )));
    }
    Ok(())
}

/// A LIKE pattern matching the strings that start with `prefix`.
pub fn prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}
