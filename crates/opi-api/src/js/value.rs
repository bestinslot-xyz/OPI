//! The JavaScript values the Express app handles: query parameters, rows
//! from node-pg and the objects handed to `JSON.stringify`.

use std::fmt;

/// A JavaScript value. Arrays hold `Undefined` for holes, which only the
/// query-string parser creates and then compacts away.
#[derive(Debug, Clone, Default)]
pub enum JsValue {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsValue>),
    Object(JsObject),
}

static UNDEFINED: JsValue = JsValue::Undefined;

impl JsValue {
    pub fn is_undefined(&self) -> bool {
        matches!(self, JsValue::Undefined)
    }

    /// `x == null`: true for `null` and `undefined`.
    pub fn is_nullish(&self) -> bool {
        matches!(self, JsValue::Undefined | JsValue::Null)
    }

    /// `typeof x === 'string'`.
    pub fn is_string(&self) -> bool {
        matches!(self, JsValue::String(_))
    }

    /// `typeof x === 'object' && x !== null`: arrays and plain objects.
    pub fn is_object(&self) -> bool {
        matches!(self, JsValue::Array(_) | JsValue::Object(_))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&JsObject> {
        match self {
            JsValue::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut JsObject> {
        match self {
            JsValue::Object(o) => Some(o),
            _ => None,
        }
    }

    /// `x[key]` for a property read: an object's own property, an array's
    /// element by canonical index, or `undefined`.
    pub fn get(&self, key: &str) -> &JsValue {
        match self {
            JsValue::Object(o) => o.get(key),
            JsValue::Array(a) => array_index(key)
                .and_then(|i| a.get(i as usize))
                .unwrap_or(&UNDEFINED),
            _ => &UNDEFINED,
        }
    }
}

/// Structural equality, for tests. `NaN` equals `NaN`; objects compare in
/// their JavaScript property order.
impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
            (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
            (JsValue::Number(a), JsValue::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
            (JsValue::String(a), JsValue::String(b)) => a == b,
            (JsValue::Array(a), JsValue::Array(b)) => a == b,
            (JsValue::Object(a), JsValue::Object(b)) => a == b,
            _ => false,
        }
    }
}

impl From<&str> for JsValue {
    fn from(s: &str) -> Self {
        JsValue::String(s.to_owned())
    }
}

impl From<String> for JsValue {
    fn from(s: String) -> Self {
        JsValue::String(s)
    }
}

impl From<f64> for JsValue {
    fn from(n: f64) -> Self {
        JsValue::Number(n)
    }
}

impl From<i32> for JsValue {
    fn from(n: i32) -> Self {
        JsValue::Number(n.into())
    }
}

impl From<u32> for JsValue {
    fn from(n: u32) -> Self {
        JsValue::Number(n.into())
    }
}

impl From<bool> for JsValue {
    fn from(b: bool) -> Self {
        JsValue::Bool(b)
    }
}

impl From<JsObject> for JsValue {
    fn from(o: JsObject) -> Self {
        JsValue::Object(o)
    }
}

impl From<Vec<JsValue>> for JsValue {
    fn from(a: Vec<JsValue>) -> Self {
        JsValue::Array(a)
    }
}

impl<T: Into<JsValue>> From<Option<T>> for JsValue {
    /// `None` is `null`.
    fn from(v: Option<T>) -> Self {
        v.map_or(JsValue::Null, Into::into)
    }
}

/// A plain JavaScript object. Properties keep their insertion order;
/// iteration and `JSON.stringify` follow the JavaScript property order, which
/// lists array-index keys first in ascending numeric order.
#[derive(Clone, Default)]
pub struct JsObject {
    entries: Vec<(String, JsValue)>,
}

impl JsObject {
    pub fn new() -> Self {
        Self::default()
    }

    /// `obj[key]`: the own property, or `undefined`.
    pub fn get(&self, key: &str) -> &JsValue {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&UNDEFINED, |(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut JsValue> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// `Object.prototype.hasOwnProperty.call(obj, key)`.
    pub fn has(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// `obj[key] = value`: replaces an existing property in place, or adds it.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<JsValue>) {
        let key = key.into();
        let value = value.into();
        match self.get_mut(&key) {
            Some(slot) => *slot = value,
            None => self.entries.push((key, value)),
        }
    }

    /// Moves an own property's value out, leaving `undefined` at its
    /// position, so a later [`set`](Self::set) keeps the key where it was.
    pub fn take(&mut self, key: &str) -> Option<JsValue> {
        self.get_mut(key).map(std::mem::take)
    }

    /// `delete obj[key]`, returning the removed value.
    pub fn remove(&mut self, key: &str) -> Option<JsValue> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `Object.keys(obj)`, in JavaScript property order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.iter().map(|(k, _)| k)
    }

    /// The properties in JavaScript property order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &JsValue)> {
        let mut indexed: Vec<(u32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, (k, _))| array_index(k).map(|n| (n, i)))
            .collect();
        indexed.sort_unstable();
        let named = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (k, _))| array_index(k).is_none())
            .map(|(i, _)| i);
        indexed
            .into_iter()
            .map(|(_, i)| i)
            .chain(named)
            .collect::<Vec<_>>()
            .into_iter()
            .map(|i| (self.entries[i].0.as_str(), &self.entries[i].1))
    }
}

impl PartialEq for JsObject {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl fmt::Debug for JsObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Into<String>, V: Into<JsValue>> FromIterator<(K, V)> for JsObject {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut obj = JsObject::new();
        for (k, v) in iter {
            obj.set(k, v);
        }
        obj
    }
}

/// The array index a property key names: a canonical decimal below
/// 2^32 - 1, as JavaScript defines it.
pub(crate) fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

/// Builds a [`JsObject`] from `key => value` pairs, in order.
#[macro_export]
macro_rules! js_object {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut obj = $crate::js::JsObject::new();
        $(obj.set($key, $value);)*
        obj
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_keys_come_first_in_numeric_order() {
        let obj = js_object! { "b" => 1, "2" => "x", "01" => true, "1" => JsValue::Null, "a" => 2 };
        assert_eq!(obj.keys().collect::<Vec<_>>(), ["1", "2", "b", "01", "a"]);
    }

    #[test]
    fn set_replaces_in_place() {
        let mut obj = js_object! { "a" => 1, "b" => 2 };
        obj.set("a", 3);
        assert_eq!(obj.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(obj.get("a"), &JsValue::from(3));
        assert!(obj.get("missing").is_undefined());
    }

    #[test]
    fn array_index_rules() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("4294967294"), Some(4294967294));
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index("01"), None);
        assert_eq!(array_index("-1"), None);
        assert_eq!(array_index(""), None);
    }
}
