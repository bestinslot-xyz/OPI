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

/// The largest `before_id` an event feed request may name.
pub const MAX_BEFORE_ID: i64 = 999_999_999_999_999_999;

/// The longest event `type` a request may name, in bytes.
pub const MAX_TYPE_LEN: usize = 256;

/// The longest `pkscript` an event feed request may name, in hex digits.
pub const MAX_PKSCRIPT_LEN: usize = 20_000;

/// The longest name the SNS indexer stores, in UTF-8 bytes; a longer
/// prefix, namespace or cursor matches nothing and is refused.
pub const MAX_NAME_LEN: usize = 2048;

/// The digits of the largest bitmap number, 2147483647.
pub const MAX_PREFIX_DIGITS: usize = 10;

/// The longest rune (an id or a spaced name) or rune name prefix a request
/// may name, in bytes.
pub const MAX_RUNE_LEN: usize = 128;

/// The longest `after` cursor of a runes list, in bytes.
pub const MAX_CURSOR_LEN: usize = 256;

/// The wallet a route lists: exactly one of `address` and `pkscript`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wallet {
    Address(String),
    PkScript(String),
}

impl Wallet {
    /// Exactly one of `address` and `pkscript`; a pkScript is lowercased hex.
    pub fn from_query(query: &JsObject) -> Result<Wallet, BadParam> {
        let address = single(query, "address")?.filter(|value| !value.is_empty());
        let pkscript = single(query, "pkscript")?.filter(|value| !value.is_empty());
        match (address, pkscript) {
            (None, None) => Err(BadParam("address or pkscript is required".into())),
            (Some(_), Some(_)) => Err(BadParam("give address or pkscript, not both".into())),
            (Some(address), None) => {
                check_text("address", address, MAX_ADDRESS_LEN)?;
                Ok(Wallet::Address(address.to_string()))
            }
            (None, Some(pkscript)) => {
                if pkscript.len() > MAX_PKSCRIPT_LEN
                    || pkscript.len() % 2 != 0
                    || !pkscript.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(BadParam(format!(
                        "pkscript must be hex of at most {MAX_PKSCRIPT_LEN} digits"
                    )));
                }
                Ok(Wallet::PkScript(pkscript.to_ascii_lowercase()))
            }
        }
    }
}

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
