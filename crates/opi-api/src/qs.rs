//! `request.query`: Express 4.18.2 parses the query string with
//! `qs.parse(str, { allowPrototypes: true })` (qs 6.11.0). This module
//! follows qs's parseValues, parseKeys, parseObject, merge and compact with
//! the default options: `&` delimiter, at most 1000 parameters, depth 5,
//! array indexes up to 20, `+` as space, and a value left undecoded when
//! decodeURIComponent throws. Array holes are `Undefined` until compaction.

use crate::js::{JsObject, JsValue, decode_uri_component, number_to_string, parse_int_str};

#[cfg(test)]
mod tests;

const PARAMETER_LIMIT: usize = 1000;
const DEPTH: usize = 5;
const ARRAY_LIMIT: f64 = 20.0;

/// Parses the raw query string (the part after `?`, if any).
pub fn parse_query(raw: Option<&str>) -> JsObject {
    let raw = match raw {
        None | Some("") => return JsObject::new(),
        Some(raw) => raw,
    };
    let values = parse_values(raw);
    let mut obj = JsValue::Object(JsObject::new());
    for (key, value) in values.iter() {
        if let Some(parsed) = parse_keys(key, value.clone()) {
            obj = merge(obj, parsed);
        }
    }
    match compact(obj) {
        JsValue::Object(obj) => obj,
        _ => unreachable!("merging into an object keeps an object"),
    }
}

/// utils.decode: `+` becomes a space, then decodeURIComponent, or the
/// string with spaces when that throws.
fn decode(s: &str) -> String {
    let spaced = s.replace('+', " ");
    decode_uri_component(&spaced).unwrap_or(spaced)
}

/// parseValues: splits the parameters into raw keys and values; a repeated
/// raw key collects its values into an array.
fn parse_values(raw: &str) -> JsObject {
    let mut obj = JsObject::new();
    for part in raw.split('&').take(PARAMETER_LIMIT) {
        let pos = match part.find("]=") {
            Some(bracket_equals) => Some(bracket_equals + 1),
            None => part.find('='),
        };
        let (key, value) = match pos {
            None => (decode(part), String::new()),
            Some(pos) => (decode(&part[..pos]), decode(&part[pos + 1..])),
        };
        // `obj.__proto__ = string` changes nothing on a plain object
        if key == "__proto__" {
            continue;
        }
        let combined = match obj.take(&key) {
            Some(existing) => combine(existing, JsValue::String(value)),
            None => JsValue::String(value),
        };
        obj.set(key, combined);
    }
    obj
}

/// `[].concat(a, b)`.
fn combine(a: JsValue, b: JsValue) -> JsValue {
    let mut items = concat_items(a);
    items.extend(concat_items(b));
    JsValue::Array(items)
}

/// The elements `[].concat(value)` adds: an array's elements, holes
/// included, or the value itself.
fn concat_items(value: JsValue) -> Vec<JsValue> {
    match value {
        JsValue::Array(items) => items,
        other => vec![other],
    }
}

/// The next `[...]` segment holding no brackets, at or after `from`, as the
/// regex `/(\[[^[\]]*])/` finds it.
fn bracket_segment(key: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = key.as_bytes();
    let mut start = from;
    while start < bytes.len() {
        if bytes[start] == b'[' {
            let mut end = start + 1;
            while end < bytes.len() && bytes[end] != b'[' && bytes[end] != b']' {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b']' {
                return Some((start, end + 1));
            }
        }
        start += 1;
    }
    None
}

/// parseKeys: splits `a[b][c]` into a parent and at most five bracket
/// segments, with any remainder as one last segment.
fn parse_keys(key: &str, value: JsValue) -> Option<JsValue> {
    if key.is_empty() {
        return None;
    }
    let first = bracket_segment(key, 0);
    let parent = match first {
        Some((start, _)) => &key[..start],
        None => key,
    };
    let mut chain: Vec<String> = Vec::new();
    if !parent.is_empty() {
        chain.push(parent.to_string());
    }
    let mut from = 0;
    let mut depth = 0;
    let mut segment;
    loop {
        segment = bracket_segment(key, from);
        let Some((start, end)) = segment else { break };
        if depth >= DEPTH {
            break;
        }
        depth += 1;
        chain.push(key[start..end].to_string());
        from = end;
    }
    if let Some((start, _)) = segment {
        chain.push(format!("[{}]", &key[start..]));
    }
    Some(parse_object(&chain, value))
}

/// parseObject: builds the nested value for one key's segments, innermost
/// first.
fn parse_object(chain: &[String], value: JsValue) -> JsValue {
    let mut leaf = value;
    for root in chain.iter().rev() {
        leaf = if root == "[]" {
            JsValue::Array(concat_items(leaf))
        } else {
            let clean = if root.len() >= 2 && root.starts_with('[') && root.ends_with(']') {
                &root[1..root.len() - 1]
            } else {
                root.as_str()
            };
            let index = parse_int_str(clean);
            if !index.is_nan()
                && root != clean
                && number_to_string(index) == clean
                && (0.0..=ARRAY_LIMIT).contains(&index)
            {
                let mut items = vec![JsValue::Undefined; index as usize + 1];
                items[index as usize] = leaf;
                JsValue::Array(items)
            } else {
                let mut obj = JsObject::new();
                if clean != "__proto__" {
                    obj.set(clean, leaf);
                }
                JsValue::Object(obj)
            }
        };
    }
    leaf
}

/// arrayToObject: an array's elements under their indexes, holes skipped.
fn array_to_object(items: Vec<JsValue>) -> JsObject {
    let mut obj = JsObject::new();
    for (i, item) in items.into_iter().enumerate() {
        if !item.is_undefined() {
            obj.set(i.to_string(), item);
        }
    }
    obj
}

/// utils.merge.
fn merge(target: JsValue, source: JsValue) -> JsValue {
    if !source.truthy() {
        return target;
    }
    if !source.is_object() {
        return match target {
            JsValue::Array(mut items) => {
                items.push(source);
                JsValue::Array(items)
            }
            JsValue::Object(mut obj) => {
                let key = source.to_js_string().unwrap_or_default();
                if key != "__proto__" {
                    obj.set(key, true);
                }
                JsValue::Object(obj)
            }
            other => JsValue::Array(vec![other, source]),
        };
    }
    if !target.is_object() {
        let mut items = vec![target];
        items.extend(concat_items(source));
        return JsValue::Array(items);
    }
    match (target, source) {
        (JsValue::Array(mut target), JsValue::Array(source)) => {
            for (i, item) in source.into_iter().enumerate() {
                if item.is_undefined() {
                    continue;
                }
                let present = target.get(i).is_some_and(|t| !t.is_undefined());
                if present {
                    if target[i].is_object() && item.is_object() {
                        let existing = std::mem::take(&mut target[i]);
                        target[i] = merge(existing, item);
                    } else {
                        target.push(item);
                    }
                } else {
                    if target.len() <= i {
                        target.resize(i + 1, JsValue::Undefined);
                    }
                    target[i] = item;
                }
            }
            JsValue::Array(target)
        }
        (target, source) => {
            let mut merge_target = match target {
                JsValue::Array(items) => array_to_object(items),
                JsValue::Object(obj) => obj,
                _ => unreachable!("target is an object"),
            };
            let entries: Vec<(String, JsValue)> = match source {
                JsValue::Object(obj) => obj
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
                JsValue::Array(items) => array_to_object(items)
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
                _ => unreachable!("source is an object"),
            };
            for (key, value) in entries {
                let merged = match merge_target.take(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                merge_target.set(key, merged);
            }
            JsValue::Object(merge_target)
        }
    }
}

/// utils.compact: drops array holes, at every depth.
fn compact(value: JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(
            items
                .into_iter()
                .filter(|item| !item.is_undefined())
                .map(compact)
                .collect(),
        ),
        JsValue::Object(obj) => JsValue::Object(
            obj.iter()
                .map(|(k, v)| (k.to_string(), compact(v.clone())))
                .collect(),
        ),
        other => other,
    }
}
