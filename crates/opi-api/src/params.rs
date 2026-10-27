//! Route parameters for sqlx's checked queries.
//!
//! node-pg sends each parameter untyped, as the text its prepareValue makes
//! of the JS value, and Postgres reads that text with the input function of
//! the type it infers for `$n`. Each helper here takes the JS value a route
//! passes, makes the same text, and reads it with that type's input rules
//! for the connected server's version ([`IntRules`], from
//! [`Db::int_rules`](crate::db::Db::int_rules)), so the macro can bind the
//! typed value. A value Postgres
//! rejects is an error, which answers the 500 the JS app sends when the
//! query fails. A missing value (`undefined`, `null`) is `None`, bound as
//! NULL. Call the helper where the JS route runs its query, so earlier
//! checks keep their order.
//!
//! No route passes a numeric, boolean or float parameter.

use {
    crate::{
        js::{JsValue, json_stringify},
        reply::ApiError,
    },
    anyhow::anyhow,
};

#[cfg(test)]
mod tests;

/// node-pg's prepareValue: the text node-pg sends for a parameter, or
/// `None` for SQL NULL.
pub fn prepare_value(value: &JsValue) -> Option<String> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Array(items) => Some(array_string(items)),
        JsValue::Object(_) => json_stringify(value),
        primitive => Some(
            primitive
                .to_js_string()
                .expect("a primitive converts to a string"),
        ),
    }
}

/// node-pg's arrayString: a Postgres array literal.
fn array_string(items: &[JsValue]) -> String {
    let elements: Vec<String> = items
        .iter()
        .map(|item| match item {
            JsValue::Undefined | JsValue::Null => "NULL".to_string(),
            JsValue::Array(inner) => array_string(inner),
            other => {
                let text = prepare_value(other).unwrap_or_default();
                format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect();
    format!("{{{}}}", elements.join(","))
}

/// The prepared text, refusing NUL as Postgres's encoding check does.
fn prepared(value: &JsValue) -> Result<Option<String>, ApiError> {
    match prepare_value(value) {
        Some(text) if text.contains('\0') => {
            Err(anyhow!("invalid byte sequence for encoding \"UTF8\": 0x00").into())
        }
        other => Ok(other),
    }
}

/// C's isspace in the "C" locale, which pg_strtoint skips around a number.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The integer input rules of the connected server's major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntRules {
    /// Postgres 15 and earlier: decimal digits only.
    Pg14,
    /// Postgres 16 and later: also `0x`, `0o` and `0b` prefixes, and `_`
    /// separators.
    Pg16,
}

impl IntRules {
    /// The rules for a server's `server_version_num` (160000 is 16.0).
    pub fn for_server_version(server_version_num: u32) -> IntRules {
        if server_version_num >= 160000 {
            IntRules::Pg16
        } else {
            IntRules::Pg14
        }
    }
}

/// pg_strtoint16/32/64: optional surrounding whitespace, an optional sign,
/// and at least one digit, within `min..=max`. Postgres 16
/// (pg_strtoint32_safe's slow path) also reads a `0x`/`0o`/`0b` prefix
/// (either case) and a `_` that a digit of the base follows; after a prefix
/// a `_` may come first, in a decimal number it may not.
fn strtoint(
    rules: IntRules,
    text: &str,
    min: i128,
    max: i128,
    type_name: &str,
) -> Result<i128, ApiError> {
    let invalid = || anyhow!("invalid input syntax for type {type_name}: \"{text}\"");
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_c_space(bytes[i]) {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let radix = match (rules, bytes.get(i), bytes.get(i + 1)) {
        (IntRules::Pg16, Some(b'0'), Some(b'x' | b'X')) => 16,
        (IntRules::Pg16, Some(b'0'), Some(b'o' | b'O')) => 8,
        (IntRules::Pg16, Some(b'0'), Some(b'b' | b'B')) => 2,
        _ => 10,
    };
    if radix != 10 {
        i += 2;
    }
    let digit = |b: Option<&u8>| b.and_then(|&b| char::from(b).to_digit(radix));
    let digits_start = i;
    let mut value: i128 = 0;
    loop {
        if let Some(d) = digit(bytes.get(i)) {
            value = value.saturating_mul(radix.into()).saturating_add(d.into());
            i += 1;
        } else if rules == IntRules::Pg16 && bytes.get(i) == Some(&b'_') {
            if radix == 10 && i == digits_start {
                return Err(invalid().into());
            }
            i += 1;
            if digit(bytes.get(i)).is_none() {
                return Err(invalid().into());
            }
        } else {
            break;
        }
    }
    if i == digits_start {
        return Err(invalid().into());
    }
    while i < bytes.len() && is_c_space(bytes[i]) {
        i += 1;
    }
    if i != bytes.len() {
        return Err(invalid().into());
    }
    let value = if negative { -value } else { value };
    if !(min..=max).contains(&value) {
        return Err(anyhow!("value \"{text}\" is out of range for type {type_name}").into());
    }
    Ok(value)
}

/// A `smallint` parameter.
pub fn int2(rules: IntRules, value: &JsValue) -> Result<Option<i16>, ApiError> {
    prepared(value)?
        .map(|t| {
            strtoint(rules, &t, i16::MIN.into(), i16::MAX.into(), "smallint").map(|v| v as i16)
        })
        .transpose()
}

/// An `integer` parameter.
pub fn int4(rules: IntRules, value: &JsValue) -> Result<Option<i32>, ApiError> {
    prepared(value)?
        .map(|t| strtoint(rules, &t, i32::MIN.into(), i32::MAX.into(), "integer").map(|v| v as i32))
        .transpose()
}

/// A `bigint` parameter (`limit $n`, `offset $n`).
pub fn int8(rules: IntRules, value: &JsValue) -> Result<Option<i64>, ApiError> {
    prepared(value)?
        .map(|t| strtoint(rules, &t, i64::MIN.into(), i64::MAX.into(), "bigint").map(|v| v as i64))
        .transpose()
}

/// A `text` parameter: the prepared text as it is.
pub fn text(value: &JsValue) -> Result<Option<String>, ApiError> {
    prepared(value)
}
