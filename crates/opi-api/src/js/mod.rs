//! The JavaScript semantics the routes rely on: values, coercions and
//! comparisons, `Number#toString`, `parseInt`, `JSON` and
//! `decodeURIComponent`.

mod json;
mod number;
mod ops;
mod uri;
mod value;

#[cfg(test)]
mod tests;

pub use {
    json::{json_parse, json_stringify},
    number::{
        js_trim, number_to_string, parse_float_str, parse_int_base10, parse_int_str,
        string_to_number,
    },
    ops::{JsError, compare_strings, sort_by_number},
    uri::decode_uri_component,
    value::{JsObject, JsValue},
};
