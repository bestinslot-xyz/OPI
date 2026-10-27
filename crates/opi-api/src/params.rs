//! Route parameters for sqlx's checked queries.
//!
//! node-pg sends each parameter untyped, as the text its prepareValue makes
//! of the JS value, and Postgres reads that text with the input function of
//! the type it infers for `$n`. Each helper here takes the JS value a route
//! passes, makes the same text, and reads it with that type's input rules
//! (Postgres 14's: decimal integers only, no `0x` prefixes or `_`
//! separators), so the macro can bind the typed value. A value Postgres
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

/// pg_strtoint16/32/64 (Postgres 14): optional surrounding whitespace, an
/// optional sign, and at least one decimal digit, within `min..=max`.
fn strtoint(text: &str, min: i128, max: i128, type_name: &str) -> Result<i128, ApiError> {
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
    let digits_start = i;
    let mut value: i128 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add(i128::from(bytes[i] - b'0'));
        i += 1;
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
pub fn int2(value: &JsValue) -> Result<Option<i16>, ApiError> {
    prepared(value)?
        .map(|t| strtoint(&t, i16::MIN.into(), i16::MAX.into(), "smallint").map(|v| v as i16))
        .transpose()
}

/// An `integer` parameter.
pub fn int4(value: &JsValue) -> Result<Option<i32>, ApiError> {
    prepared(value)?
        .map(|t| strtoint(&t, i32::MIN.into(), i32::MAX.into(), "integer").map(|v| v as i32))
        .transpose()
}

/// A `bigint` parameter (`limit $n`, `offset $n`).
pub fn int8(value: &JsValue) -> Result<Option<i64>, ApiError> {
    prepared(value)?
        .map(|t| strtoint(&t, i64::MIN.into(), i64::MAX.into(), "bigint").map(|v| v as i64))
        .transpose()
}

/// A `text` parameter: the prepared text as it is.
pub fn text(value: &JsValue) -> Result<Option<String>, ApiError> {
    prepared(value)
}
