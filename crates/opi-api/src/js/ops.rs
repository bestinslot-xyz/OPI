//! JavaScript coercions and operators on [`JsValue`]: truthiness, `||`,
//! `String(x)`, `x + y`, `==`, `<`, `>`, `<=`, `>=`, `parseInt(x)` and the
//! string methods the routes call.

use {
    super::{
        JsValue,
        number::{number_to_string, parse_int_str, string_to_number},
    },
    std::{cmp::Ordering, fmt},
};

/// A JavaScript exception (`TypeError`, `SyntaxError`). Route code returns
/// it where the JavaScript line throws, which the app answers with a 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsError {
    pub kind: &'static str,
    pub message: String,
}

impl JsError {
    pub fn type_error(message: impl Into<String>) -> Self {
        JsError {
            kind: "TypeError",
            message: message.into(),
        }
    }

    pub fn syntax_error(message: impl Into<String>) -> Self {
        JsError {
            kind: "SyntaxError",
            message: message.into(),
        }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for JsError {}

impl JsValue {
    /// ToBoolean.
    pub fn truthy(&self) -> bool {
        match self {
            JsValue::Undefined | JsValue::Null => false,
            JsValue::Bool(b) => *b,
            JsValue::Number(n) => !(n.is_nan() || *n == 0.0),
            JsValue::String(s) => !s.is_empty(),
            JsValue::Array(_) | JsValue::Object(_) => true,
        }
    }

    /// `self || other`.
    pub fn or(self, other: JsValue) -> JsValue {
        if self.truthy() { self } else { other }
    }

    /// ToPrimitive. An array becomes `join(',')`; a plain object becomes
    /// `"[object Object]"` unless it has an own `toString` property, which
    /// (never being a function here) makes the conversion throw.
    pub fn to_primitive(&self) -> Result<JsValue, JsError> {
        match self {
            JsValue::Array(items) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(if item.is_nullish() {
                        String::new()
                    } else {
                        item.to_js_string()?
                    });
                }
                Ok(JsValue::String(parts.join(",")))
            }
            JsValue::Object(obj) => {
                if obj.has("toString") {
                    Err(JsError::type_error(
                        "Cannot convert object to primitive value",
                    ))
                } else {
                    Ok(JsValue::String("[object Object]".into()))
                }
            }
            primitive => Ok(primitive.clone()),
        }
    }

    /// ToString, as `String(x)` and `x + ''` apply it.
    pub fn to_js_string(&self) -> Result<String, JsError> {
        Ok(match self {
            JsValue::Undefined => "undefined".into(),
            JsValue::Null => "null".into(),
            JsValue::Bool(b) => b.to_string(),
            JsValue::Number(n) => number_to_string(*n),
            JsValue::String(s) => s.clone(),
            JsValue::Array(_) | JsValue::Object(_) => return self.to_primitive()?.to_js_string(),
        })
    }

    /// ToNumber.
    pub fn to_number(&self) -> Result<f64, JsError> {
        Ok(match self {
            JsValue::Undefined => f64::NAN,
            JsValue::Null => 0.0,
            JsValue::Bool(b) => f64::from(u8::from(*b)),
            JsValue::Number(n) => *n,
            JsValue::String(s) => string_to_number(s),
            JsValue::Array(_) | JsValue::Object(_) => return self.to_primitive()?.to_number(),
        })
    }

    /// `parseInt(self)`.
    pub fn parse_int(&self) -> Result<f64, JsError> {
        Ok(parse_int_str(&self.to_js_string()?))
    }

    /// `self == other` (IsLooselyEqual). Two objects are never the same
    /// reference here, so they are unequal.
    pub fn loose_eq(&self, other: &JsValue) -> Result<bool, JsError> {
        use JsValue::*;
        Ok(match (self, other) {
            (Undefined | Null, Undefined | Null) => true,
            (Undefined | Null, _) | (_, Undefined | Null) => false,
            (Number(a), Number(b)) => a == b,
            (String(a), String(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (Number(a), String(b)) => *a == string_to_number(b),
            (String(a), Number(b)) => string_to_number(a) == *b,
            (Bool(_), _) => return Number(self.to_number()?).loose_eq(other),
            (_, Bool(_)) => return self.loose_eq(&Number(other.to_number()?)),
            (Number(_) | String(_), Array(_) | Object(_)) => {
                return self.loose_eq(&other.to_primitive()?);
            }
            (Array(_) | Object(_), Number(_) | String(_)) => {
                return self.to_primitive()?.loose_eq(other);
            }
            (Array(_) | Object(_), Array(_) | Object(_)) => false,
        })
    }

    /// `self > other`.
    pub fn gt(&self, other: &JsValue) -> Result<bool, JsError> {
        Ok(is_less_than(other, self, false)? == Some(true))
    }

    /// `self < other`.
    pub fn lt(&self, other: &JsValue) -> Result<bool, JsError> {
        Ok(is_less_than(self, other, true)? == Some(true))
    }

    /// `self >= other`.
    pub fn ge(&self, other: &JsValue) -> Result<bool, JsError> {
        Ok(is_less_than(self, other, true)? == Some(false))
    }

    /// `self <= other`.
    pub fn le(&self, other: &JsValue) -> Result<bool, JsError> {
        Ok(is_less_than(other, self, false)? == Some(false))
    }

    /// `self + other`: string concatenation when either primitive is a
    /// string, numeric addition otherwise.
    pub fn add(&self, other: &JsValue) -> Result<JsValue, JsError> {
        let left = self.to_primitive()?;
        let right = other.to_primitive()?;
        if left.is_string() || right.is_string() {
            Ok(JsValue::String(
                left.to_js_string()? + &right.to_js_string()?,
            ))
        } else {
            Ok(JsValue::Number(left.to_number()? + right.to_number()?))
        }
    }

    /// `self.toLowerCase()`, which only strings have.
    pub fn to_lower_case(&self) -> Result<String, JsError> {
        self.string_method("toLowerCase").map(str::to_lowercase)
    }

    /// `self.toUpperCase()`, which only strings have.
    pub fn to_upper_case(&self) -> Result<String, JsError> {
        self.string_method("toUpperCase").map(str::to_uppercase)
    }

    fn string_method(&self, name: &str) -> Result<&str, JsError> {
        match self {
            JsValue::String(s) => Ok(s),
            JsValue::Undefined | JsValue::Null => Err(JsError::type_error(format!(
                "Cannot read properties of {} (reading '{name}')",
                if self.is_undefined() {
                    "undefined"
                } else {
                    "null"
                }
            ))),
            _ => Err(JsError::type_error(format!(
                "value.{name} is not a function"
            ))),
        }
    }
}

/// IsLessThan: `None` stands for undefined (a NaN on either side).
fn is_less_than(x: &JsValue, y: &JsValue, left_first: bool) -> Result<Option<bool>, JsError> {
    let (px, py) = if left_first {
        let px = x.to_primitive()?;
        (px, y.to_primitive()?)
    } else {
        let py = y.to_primitive()?;
        (x.to_primitive()?, py)
    };
    if let (JsValue::String(a), JsValue::String(b)) = (&px, &py) {
        return Ok(Some(compare_strings(a, b) == Ordering::Less));
    }
    let (nx, ny) = (px.to_number()?, py.to_number()?);
    if nx.is_nan() || ny.is_nan() {
        return Ok(None);
    }
    Ok(Some(nx < ny))
}

/// JavaScript string order: UTF-16 code unit by code unit.
pub fn compare_strings(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `items.sort((a, b) => cmp(a, b))`: a stable sort where a negative result
/// orders `a` first, a positive one `b` first, and 0 or NaN keeps them in
/// order. For comparators that are consistent, every stable sort, V8's
/// TimSort among them, gives this order.
pub fn sort_by_number<T>(items: &mut [T], mut cmp: impl FnMut(&T, &T) -> f64) {
    let mut order: Vec<usize> = (0..items.len()).collect();
    let mut scratch = order.clone();
    merge_sort(&mut order, &mut scratch, &mut |&a, &b| {
        let r = cmp(&items[a], &items[b]);
        if r < 0.0 {
            Ordering::Less
        } else if r > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    });
    // place items[order[i]] at i
    for i in 0..order.len() {
        let mut j = order[i];
        while j < i {
            j = order[j];
        }
        items.swap(i, j);
    }
}

fn merge_sort(
    v: &mut [usize],
    scratch: &mut [usize],
    cmp: &mut impl FnMut(&usize, &usize) -> Ordering,
) {
    let len = v.len();
    if len <= 1 {
        return;
    }
    let mid = len / 2;
    merge_sort(&mut v[..mid], &mut scratch[..mid], cmp);
    merge_sort(&mut v[mid..], &mut scratch[mid..], cmp);
    let (mut i, mut j, mut k) = (0, mid, 0);
    while i < mid && j < len {
        // take from the right only when it orders strictly first
        if cmp(&v[j], &v[i]) == Ordering::Less {
            scratch[k] = v[j];
            j += 1;
        } else {
            scratch[k] = v[i];
            i += 1;
        }
        k += 1;
    }
    scratch[k..k + (mid - i)].copy_from_slice(&v[i..mid]);
    k += mid - i;
    scratch[k..k + (len - j)].copy_from_slice(&v[j..len]);
    v.copy_from_slice(&scratch[..len]);
}
