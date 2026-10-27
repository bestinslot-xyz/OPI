//! Postgres 14's input functions (int2in/int4in/int8in via pg_strtoint*,
//! textin) on the text node-pg's prepareValue makes of each JS value.

use {
    super::*,
    crate::{js::JsValue, js_object},
};

fn s(v: &str) -> JsValue {
    JsValue::from(v)
}

#[test]
fn prepare_value_matches_node_pg() {
    // Expected values are JSON.stringify(prepareValue(v)) from pg 8.23.
    let cases: Vec<(JsValue, Option<&str>)> = vec![
        (JsValue::Undefined, None),
        (JsValue::Null, None),
        (s("x"), Some("x")),
        (JsValue::Bool(true), Some("true")),
        (JsValue::Number(51.0), Some("51")),
        (JsValue::Number(1e21), Some("1e+21")),
        (
            JsValue::Array(vec![
                s("a"),
                s("b\"c\\d"),
                JsValue::Null,
                JsValue::Array(vec![s("x")]),
                js_object! { "k" => "v" }.into(),
            ]),
            Some(r#"{"a","b\"c\\d",NULL,{"x"},"{\"k\":\"v\"}"}"#),
        ),
        (
            js_object! { "b" => "", "2" => "x" }.into(),
            Some(r#"{"2":"x","b":""}"#),
        ),
        (JsValue::Array(vec![]), Some("{}")),
    ];
    for (value, expected) in cases {
        assert_eq!(prepare_value(&value).as_deref(), expected, "{value:?}");
    }
}

#[test]
fn int4_follows_int4in() {
    let ok: &[(&str, i32)] = &[
        ("5", 5),
        (" \t\n+5 \r\u{b}\u{c}", 5),
        ("-2147483648", -2147483648),
        ("2147483647", 2147483647),
        ("007", 7),
    ];
    for &(text, want) in ok {
        assert_eq!(int4(&s(text)).unwrap(), Some(want), "{text:?}");
    }
    for bad in [
        "",
        " ",
        "1.5",
        "1e3",
        "0x10",
        "abc",
        "5px",
        "+",
        "- 5",
        "2147483648",
        "1_000",
        "\u{a0}5",
        "5\0",
    ] {
        assert!(int4(&s(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn js_values_reach_the_input_function_as_node_pg_sends_them() {
    // a missing param is NULL
    assert_eq!(int4(&JsValue::Undefined).unwrap(), None);
    // a numeric JS value goes through Number#toString
    assert_eq!(int8(&JsValue::Number(51.0)).unwrap(), Some(51));
    assert!(int8(&JsValue::Number(1e21)).is_err());
    // a repeated key is an array literal, an `a[b]=` object is JSON; no
    // integer reads either
    assert!(int4(&JsValue::Array(vec![s("5"), s("6")])).is_err());
    assert!(int4(&js_object! { "b" => "" }.into()).is_err());
    // the error is the 500 answer's cause, with Postgres's message
    let error = int4(&s("1.5")).unwrap_err();
    assert_eq!(
        error.to_string(),
        r#"invalid input syntax for type integer: "1.5""#
    );
    assert_eq!(
        int2(&s("40000")).unwrap_err().to_string(),
        r#"value "40000" is out of range for type smallint"#
    );
}

#[test]
fn int2_and_int8_ranges() {
    assert_eq!(int2(&s("-32768")).unwrap(), Some(-32768));
    assert!(int2(&s("32768")).is_err());
    assert_eq!(int8(&s("-9223372036854775808")).unwrap(), Some(i64::MIN));
    assert!(int8(&s("9223372036854775808")).is_err());
}

#[test]
fn text_is_the_prepared_text() {
    assert_eq!(text(&s(" a ")).unwrap().as_deref(), Some(" a "));
    assert_eq!(text(&JsValue::Undefined).unwrap(), None);
    assert_eq!(
        text(&JsValue::Array(vec![s("a"), s("b")]))
            .unwrap()
            .as_deref(),
        Some(r#"{"a","b"}"#)
    );
    // Postgres text cannot hold NUL
    assert!(text(&s("a\0b")).is_err());
}
