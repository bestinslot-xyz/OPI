//! Postgres's input functions (int2in/int4in/int8in via pg_strtoint*,
//! textin), as Postgres 14 and 16 read them, on the text node-pg's
//! prepareValue makes of each JS value.

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

const PG14: IntRules = IntRules::Pg14;
const PG16: IntRules = IntRules::Pg16;

/// Input both rule sets read alike.
#[test]
fn int4_follows_int4in_on_every_version() {
    let ok: &[(&str, i32)] = &[
        ("5", 5),
        (" \t\n+5 \r\u{b}\u{c}", 5),
        ("-2147483648", -2147483648),
        ("2147483647", 2147483647),
        ("007", 7),
    ];
    let bad = [
        "",
        " ",
        "1.5",
        "1e3",
        "abc",
        "5px",
        "+",
        "- 5",
        "2147483648",
        "\u{a0}5",
        "5\0",
    ];
    for rules in [PG14, PG16] {
        for &(text, want) in ok {
            assert_eq!(
                int4(rules, &s(text)).unwrap(),
                Some(want),
                "{rules:?} {text:?}"
            );
        }
        for text in bad {
            assert!(int4(rules, &s(text)).is_err(), "{rules:?} {text:?}");
        }
    }
}

#[test]
fn postgres_14_reads_decimal_digits_only() {
    for text in ["0x10", "0o17", "0b101", "1_000", "0X1F"] {
        assert!(int4(PG14, &s(text)).is_err(), "{text:?}");
    }
}

#[test]
fn postgres_16_reads_prefixes_and_underscores() {
    // pg_strtoint32_safe (REL_16_STABLE): 0x/0o/0b prefixes, and `_`
    // followed by a digit of the base; a decimal number may not start with
    // `_`, but a `_` may follow a prefix
    let ok: &[(&str, i32)] = &[
        ("0x10", 16),
        ("0XfF", 255),
        ("-0x10", -16),
        (" +0o17 ", 15),
        ("0O17", 15),
        ("0b101", 5),
        ("0B1", 1),
        ("1_000", 1000),
        ("1_0_0", 100),
        ("0x_1f", 31),
        ("0b_1_0", 2),
        ("-0x80000000", i32::MIN),
        ("0x7fff_ffff", i32::MAX),
    ];
    for &(text, want) in ok {
        assert_eq!(int4(PG16, &s(text)).unwrap(), Some(want), "{text:?}");
    }
    for text in [
        "_1",
        "1_",
        "1__0",
        "1_ 0",
        "0x",
        "0x_",
        "0xg",
        "0o8",
        "0b2",
        "0x1_",
        "0b_",
        "- 0x1",
        "0 x1",
        "00x1",
        "0x80000000",
        "+_1",
    ] {
        assert!(int4(PG16, &s(text)).is_err(), "{text:?}");
    }
    assert_eq!(
        int2(PG16, &s("0x8000")).unwrap_err().to_string(),
        r#"value "0x8000" is out of range for type smallint"#
    );
    assert_eq!(
        int8(PG16, &s("-0x8000000000000000")).unwrap(),
        Some(i64::MIN)
    );
}

#[test]
fn the_rules_follow_the_server_version() {
    assert_eq!(IntRules::for_server_version(140015), PG14);
    assert_eq!(IntRules::for_server_version(150008), PG14);
    assert_eq!(IntRules::for_server_version(160000), PG16);
    assert_eq!(IntRules::for_server_version(170002), PG16);
}

#[test]
fn js_values_reach_the_input_function_as_node_pg_sends_them() {
    // a missing param is NULL
    assert_eq!(int4(PG14, &JsValue::Undefined).unwrap(), None);
    // a numeric JS value goes through Number#toString
    assert_eq!(int8(PG14, &JsValue::Number(51.0)).unwrap(), Some(51));
    assert!(int8(PG16, &JsValue::Number(1e21)).is_err());
    // a repeated key is an array literal, an `a[b]=` object is JSON; no
    // integer reads either
    assert!(int4(PG16, &JsValue::Array(vec![s("5"), s("6")])).is_err());
    assert!(int4(PG16, &js_object! { "b" => "" }.into()).is_err());
    // the error is the 500 answer's cause, with Postgres's message
    let error = int4(PG14, &s("1.5")).unwrap_err();
    assert_eq!(
        error.to_string(),
        r#"invalid input syntax for type integer: "1.5""#
    );
    assert_eq!(
        int2(PG14, &s("40000")).unwrap_err().to_string(),
        r#"value "40000" is out of range for type smallint"#
    );
}

#[test]
fn int2_and_int8_ranges() {
    for rules in [PG14, PG16] {
        assert_eq!(int2(rules, &s("-32768")).unwrap(), Some(-32768));
        assert!(int2(rules, &s("32768")).is_err());
        assert_eq!(
            int8(rules, &s("-9223372036854775808")).unwrap(),
            Some(i64::MIN)
        );
        assert!(int8(rules, &s("9223372036854775808")).is_err());
    }
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
