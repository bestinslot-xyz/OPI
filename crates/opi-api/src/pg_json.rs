//! Postgres values in the form node-pg 8 (pg-types 2.2.0 text parsers)
//! hands them to the routes. A typed column sqlx decodes maps through
//! [`ToJs`]; a column a route casts to text decodes with [`text`] (or
//! [`decode_text`]) by these rules:
//!
//! - int2, int4, oid: `parseInt(text, 10)`; float4, float8: `parseFloat`.
//! - int8, numeric, text, varchar, bpchar, name: the text as a string.
//! - bool: true for `TRUE t true y yes on 1`.
//! - json, jsonb: `JSON.parse`.
//! - date, timestamp, timestamptz: a JS `Date`, which reaches the client as
//!   its `toJSON()` ISO string, or `null` for an invalid date; `±infinity`
//!   is the number `±Infinity`. A date or a timestamp without a zone reads
//!   as local time.
//! - the arrays pg-types registers for those types, parsed by
//!   postgres-array 2.0.0 (numeric[] elements go through `parseFloat`).
//!
//! Any other type is [`PgJsonError::UnsupportedType`]; `NULL` is `null`
//! whatever the type.

use {
    crate::js::{JsValue, json_parse, parse_float_str, parse_int_base10},
    jiff::{Timestamp, tz::TimeZone},
    std::{fmt, sync::LazyLock},
};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub enum PgJsonError {
    /// No node-pg rule for this type OID is implemented.
    UnsupportedType(u32),
    /// The text does not parse as the type (`JSON.parse` throws, an array
    /// is unbalanced).
    Malformed(u32, String),
}

impl fmt::Display for PgJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgJsonError::UnsupportedType(oid) => {
                write!(f, "no node-pg rule for the Postgres type with OID {oid}")
            }
            PgJsonError::Malformed(oid, why) => write!(f, "OID {oid} value: {why}"),
        }
    }
}

impl std::error::Error for PgJsonError {}

/// How pg-types parses one value (or one array element).
#[derive(Clone, Copy)]
enum Scalar {
    Int,
    BigInt,
    Float,
    Bool,
    Text,
    Json,
    Date,
}

fn rule(oid: u32) -> Option<(Scalar, bool)> {
    use Scalar::*;
    Some(match oid {
        21 | 23 | 26 => (Int, false),
        20 => (BigInt, false),
        700 | 701 => (Float, false),
        16 => (Bool, false),
        25 | 1043 | 1042 | 19 | 1700 => (Text, false),
        114 | 3802 => (Json, false),
        1082 | 1114 | 1184 => (Date, false),
        1182 => (Date, true),
        1005 | 1007 => (Int, true),
        1016 => (BigInt, true),
        1021 | 1022 | 1231 => (Float, true),
        1000 => (Bool, true),
        1009 | 1015 | 1014 => (Text, true),
        199 | 3807 => (Json, true),
        1115 | 1185 => (Date, true),
        _ => return None,
    })
}

/// Decodes one text-format value of the type `oid`.
pub fn decode_text(oid: u32, text: Option<&str>, local: &TimeZone) -> Result<JsValue, PgJsonError> {
    let Some(text) = text else {
        return Ok(JsValue::Null);
    };
    let (scalar, is_array) = rule(oid).ok_or(PgJsonError::UnsupportedType(oid))?;
    let element = |entry: &str, in_array: bool| -> Result<JsValue, PgJsonError> {
        Ok(match scalar {
            Scalar::Int => JsValue::Number(parse_int_base10(entry)),
            // parseBigIntegerArray trims its elements
            Scalar::BigInt if in_array => JsValue::String(entry.trim().to_string()),
            Scalar::BigInt | Scalar::Text => JsValue::String(entry.to_string()),
            Scalar::Float => JsValue::Number(parse_float_str(entry)),
            Scalar::Bool => JsValue::Bool(matches!(
                entry,
                "TRUE" | "t" | "true" | "y" | "yes" | "on" | "1"
            )),
            Scalar::Json => {
                json_parse(entry).map_err(|e| PgJsonError::Malformed(oid, e.to_string()))?
            }
            Scalar::Date => parse_date(entry, local),
        })
    };
    if !is_array {
        return element(text, false);
    }
    // the array parsers answer null for an empty string
    if text.is_empty() {
        return Ok(JsValue::Null);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut transform = |entry: &str| element(entry, true);
    let (entries, _) = parse_array(&chars, &mut transform, false).map_err(|e| match e {
        ArrayError::Unbalanced => {
            PgJsonError::Malformed(oid, "array dimension not balanced".into())
        }
        ArrayError::Element(e) => e,
    })?;
    Ok(JsValue::Array(entries))
}

enum ArrayError {
    Unbalanced,
    Element(PgJsonError),
}

/// postgres-array 2.0.0's ArrayParser#parse, character for character. It
/// answers the entries and the position it stopped at.
fn parse_array(
    source: &[char],
    transform: &mut dyn FnMut(&str) -> Result<JsValue, PgJsonError>,
    nested: bool,
) -> Result<(Vec<JsValue>, usize), ArrayError> {
    let mut position = 0;
    let mut entries = Vec::new();
    // the recorded text, and whether anything was recorded: a trailing
    // backslash records `undefined`, which counts but joins as ""
    let mut recorded = (String::new(), false);
    let mut dimension = 0;
    let mut quote = false;

    let new_entry = |recorded: &mut (String, bool),
                     entries: &mut Vec<JsValue>,
                     include_empty: bool,
                     transform: &mut dyn FnMut(&str) -> Result<JsValue, PgJsonError>|
     -> Result<(), ArrayError> {
        if recorded.1 || include_empty {
            let entry = std::mem::take(&mut recorded.0);
            recorded.1 = false;
            let value = if entry == "NULL" && !include_empty {
                JsValue::Null
            } else {
                transform(&entry).map_err(ArrayError::Element)?
            };
            entries.push(value);
        }
        Ok(())
    };

    // nextCharacter: a backslash yields the next character, escaped
    let next = |position: &mut usize| -> (Option<char>, bool) {
        let c = source[*position];
        *position += 1;
        if c == '\\' {
            let value = source.get(*position).copied();
            *position += 1;
            (value, true)
        } else {
            (Some(c), false)
        }
    };

    // consumeDimensions: skip a `[1:2]=` prefix
    if source.first() == Some(&'[') {
        while position < source.len() {
            if next(&mut position).0 == Some('=') {
                break;
            }
        }
    }
    while position < source.len() {
        let (value, escaped) = next(&mut position);
        let Some(c) = value else {
            recorded.1 = true;
            continue;
        };
        if c == '{' && !quote {
            dimension += 1;
            if dimension > 1 {
                let (inner, consumed) = parse_array(&source[position - 1..], transform, true)?;
                entries.push(JsValue::Array(inner));
                position = position + consumed - 2;
            }
        } else if c == '}' && !quote {
            dimension -= 1;
            if dimension == 0 {
                new_entry(&mut recorded, &mut entries, false, transform)?;
                if nested {
                    return Ok((entries, position));
                }
            }
        } else if c == '"' && !escaped {
            if quote {
                new_entry(&mut recorded, &mut entries, true, transform)?;
            }
            quote = !quote;
        } else if c == ',' && !quote {
            new_entry(&mut recorded, &mut entries, false, transform)?;
        } else {
            recorded.0.push(c);
            recorded.1 = true;
        }
    }
    if dimension != 0 {
        return Err(ArrayError::Unbalanced);
    }
    Ok((entries, position))
}

const MS_PER_DAY: f64 = 86_400_000.0;
/// TimeClip's bound.
const MAX_TIME: f64 = 8.64e15;

/// postgres-date 1.0.7 followed by `Date#toJSON`.
fn parse_date(text: &str, local: &TimeZone) -> JsValue {
    if text == "infinity" {
        return JsValue::Number(f64::INFINITY);
    }
    if text == "-infinity" {
        return JsValue::Number(f64::NEG_INFINITY);
    }
    let Some(parts) = DateTimeParts::parse(text) else {
        return parse_date_only(text, local);
    };
    let year = if parts.bc {
        -(parts.year - 1.0)
    } else {
        parts.year
    };
    let ms = parts.fraction.map_or(0.0, |f| 1000.0 * parse_float_str(f));
    let wall = make_date(
        year,
        parts.month - 1.0,
        parts.day,
        make_time(parts.hour, parts.minute, parts.second, ms),
    );
    let time = match time_zone_offset(text) {
        // Date.UTC clips before the offset applies
        Some(_) if wall.abs() > MAX_TIME => f64::NAN,
        Some(offset) => wall - offset,
        None => local_to_utc(wall, local),
    };
    date_to_json(time)
}

/// `^(\d{1,})-(\d{2})-(\d{2})( BC)?$` as a local midnight, or null.
fn parse_date_only(text: &str, local: &TimeZone) -> JsValue {
    let (body, bc) = match text.strip_suffix(" BC") {
        Some(body) => (body, true),
        None => (text, false),
    };
    let mut fields = body.splitn(3, '-');
    let (Some(y), Some(m), Some(d)) = (fields.next(), fields.next(), fields.next()) else {
        return JsValue::Null;
    };
    let digits = |s: &str, len: Option<usize>| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && len.is_none_or(|l| s.len() == l)
    };
    if !digits(y, None) || !digits(m, Some(2)) || !digits(d, Some(2)) {
        return JsValue::Null;
    }
    let mut year = parse_int_base10(y);
    if bc {
        year = -(year - 1.0);
    }
    let wall = make_date(year, parse_int_base10(m) - 1.0, parse_int_base10(d), 0.0);
    date_to_json(local_to_utc(wall, local))
}

/// The fields of postgres-date's DATE_TIME pattern:
/// `(\d{1,})-(\d{2})-(\d{2}) (\d{2}):(\d{2}):(\d{2})(\.\d{1,})?.*?( BC)?$`.
struct DateTimeParts<'a> {
    year: f64,
    month: f64,
    day: f64,
    hour: f64,
    minute: f64,
    second: f64,
    fraction: Option<&'a str>,
    bc: bool,
}

impl<'a> DateTimeParts<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let b = text.as_bytes();
        let digits_at = |i: usize, n: usize| -> Option<f64> {
            let s = text.get(i..i + n)?;
            s.bytes()
                .all(|c| c.is_ascii_digit())
                .then(|| parse_int_base10(s))
        };
        let year_len = b.iter().take_while(|c| c.is_ascii_digit()).count();
        if year_len == 0 {
            return None;
        }
        let mut i = year_len;
        let expect = |i: usize, c: u8| b.get(i) == Some(&c);
        if !expect(i, b'-') {
            return None;
        }
        let month = digits_at(i + 1, 2)?;
        i += 3;
        if !expect(i, b'-') {
            return None;
        }
        let day = digits_at(i + 1, 2)?;
        i += 3;
        if !expect(i, b' ') {
            return None;
        }
        let hour = digits_at(i + 1, 2)?;
        i += 3;
        if !expect(i, b':') {
            return None;
        }
        let minute = digits_at(i + 1, 2)?;
        i += 3;
        if !expect(i, b':') {
            return None;
        }
        let second = digits_at(i + 1, 2)?;
        i += 3;
        let fraction = if expect(i, b'.') {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            (n > 0).then(|| &text[i..i + 1 + n])
        } else {
            None
        };
        Some(DateTimeParts {
            year: parse_int_base10(&text[..year_len]),
            month,
            day,
            hour,
            minute,
            second,
            fraction,
            bc: text.ends_with(" BC"),
        })
    }
}

/// postgres-date's timeZoneOffset in milliseconds; `None` when the text
/// names no zone.
fn time_zone_offset(text: &str) -> Option<f64> {
    if text.ends_with("+00") {
        return Some(0.0);
    }
    let time = text.split(' ').nth(1)?;
    let start = time.find(['Z', '+', '-'])?;
    let zone = &time[start..];
    if zone.starts_with('Z') {
        return Some(0.0);
    }
    let sign = if zone.starts_with('-') { -1.0 } else { 1.0 };
    // ([Z+-])(\d{2})?:?(\d{2})?:?(\d{2})?
    let rest = &zone[1..];
    let mut groups: [Option<&str>; 3] = [None; 3];
    let mut pos = 0;
    for (g, group) in groups.iter_mut().enumerate() {
        if g > 0 && rest[pos..].starts_with(':') {
            pos += 1;
        }
        let two = rest
            .get(pos..pos + 2)
            .filter(|s| s.bytes().all(|c| c.is_ascii_digit()));
        if let Some(two) = two {
            *group = Some(two);
            pos += 2;
        }
    }
    let int = |g: Option<&str>| g.map_or(f64::NAN, parse_int_base10);
    let or_zero = |g: Option<&str>| g.map_or(0.0, parse_int_base10);
    let seconds = int(groups[0]) * 3600.0 + or_zero(groups[1]) * 60.0 + or_zero(groups[2]);
    Some(seconds * sign * 1000.0)
}

/// ECMAScript MakeTime, truncating each field.
fn make_time(hour: f64, minute: f64, second: f64, ms: f64) -> f64 {
    hour.trunc() * 3_600_000.0 + minute.trunc() * 60_000.0 + second.trunc() * 1000.0 + ms.trunc()
}

/// ECMAScript MakeDate(MakeDay(year, month, day), time) for the proleptic
/// Gregorian calendar.
fn make_date(year: f64, month: f64, day: f64, time: f64) -> f64 {
    let y = year + (month / 12.0).floor();
    let m = month.rem_euclid(12.0) + 1.0;
    days_from_civil(y as i64, m as i64) as f64 * MS_PER_DAY + (day - 1.0) * MS_PER_DAY + time
}

/// Days from 1970-01-01 to the first of `month` (1-12) in `year`.
fn days_from_civil(year: i64, month: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The (year, month 1-12, day) of a day count from 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// ECMAScript UTC(t) for a local wall-clock time: a time a gap skips moves
/// forward, a time a fold repeats takes the earlier instant (jiff's
/// "compatible" disambiguation). Outside jiff's range (years ±9999) the
/// offset at the range's end applies.
fn local_to_utc(wall: f64, local: &TimeZone) -> f64 {
    if !wall.is_finite() {
        return f64::NAN;
    }
    let as_utc = Timestamp::from_millisecond(wall as i64).ok();
    let resolved = as_utc
        .map(|ts| TimeZone::UTC.to_datetime(ts))
        .and_then(|dt| local.to_ambiguous_timestamp(dt).compatible().ok());
    match resolved {
        Some(ts) => ts.as_millisecond() as f64,
        None => {
            let edge = if wall < 0.0 {
                Timestamp::MIN
            } else {
                Timestamp::MAX
            };
            wall - f64::from(local.to_offset(edge).seconds()) * 1000.0
        }
    }
}

/// `Date#toJSON`: the toISOString text, or null for an invalid date.
fn date_to_json(time: f64) -> JsValue {
    if !time.is_finite() || time.abs() > MAX_TIME {
        return JsValue::Null;
    }
    let ms = time as i64;
    let days = ms.div_euclid(86_400_000);
    let in_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    };
    JsValue::String(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000
    ))
}

/// The type OIDs of the columns a route casts to text.
pub mod oid {
    pub const JSON: u32 = 114;
    pub const FLOAT4: u32 = 700;
    pub const FLOAT8: u32 = 701;
    pub const DATE: u32 = 1082;
    pub const TIMESTAMP: u32 = 1114;
    pub const TIMESTAMPTZ: u32 = 1184;
    pub const NUMERIC: u32 = 1700;
    pub const NUMERIC_ARRAY: u32 = 1231;
    pub const JSONB: u32 = 3802;
}

static LOCAL: LazyLock<TimeZone> = LazyLock::new(TimeZone::system);

/// A column cast to text (`col::text as col`) as node-pg reads a column
/// of the type `oid`. Timestamps without a zone and dates read in the
/// process's local time zone (TZ), as node does.
pub fn text(oid: u32, text: Option<&str>) -> Result<JsValue, PgJsonError> {
    decode_text(oid, text, &LOCAL)
}

/// A value sqlx decodes from binary, in the form node-pg gives it.
///
/// - `i16`, `i32`, `f64`: a number (`parseInt`/`parseFloat` of Postgres's
///   text give the same value). `f32`: `parseFloat` of Postgres's shortest
///   float4 text, so `0.1::float4` is `0.1`.
/// - `i64`: its decimal text, as node-pg leaves int8.
/// - `bool`, `String`: as they are. `Option`: `None` is null. `Vec`: an
///   array of the elements' forms.
///
/// numeric, json, jsonb, date and timestamps have no typed form here: a
/// route casts them to text and decodes them with [`text`], since binary
/// decoding loses numeric's scale and NaN, jsonb's key order, and the text
/// node-pg's date parser reads.
pub trait ToJs {
    fn to_js(self) -> JsValue;
}

impl ToJs for i16 {
    fn to_js(self) -> JsValue {
        JsValue::Number(self.into())
    }
}

impl ToJs for i32 {
    fn to_js(self) -> JsValue {
        JsValue::Number(self.into())
    }
}

impl ToJs for i64 {
    fn to_js(self) -> JsValue {
        JsValue::String(self.to_string())
    }
}

impl ToJs for f64 {
    fn to_js(self) -> JsValue {
        JsValue::Number(self)
    }
}

impl ToJs for f32 {
    fn to_js(self) -> JsValue {
        if !self.is_finite() {
            return JsValue::Number(self.into());
        }
        // Rust prints an f32's shortest round-tripping digits, as
        // Postgres's float4out does
        JsValue::Number(
            self.to_string()
                .parse()
                .expect("a finite f32 prints a number"),
        )
    }
}

impl ToJs for bool {
    fn to_js(self) -> JsValue {
        JsValue::Bool(self)
    }
}

impl ToJs for String {
    fn to_js(self) -> JsValue {
        JsValue::String(self)
    }
}

impl ToJs for &str {
    fn to_js(self) -> JsValue {
        JsValue::from(self)
    }
}

impl<T: ToJs> ToJs for Option<T> {
    fn to_js(self) -> JsValue {
        self.map_or(JsValue::Null, ToJs::to_js)
    }
}

impl<T: ToJs> ToJs for Vec<T> {
    fn to_js(self) -> JsValue {
        JsValue::Array(self.into_iter().map(ToJs::to_js).collect())
    }
}
