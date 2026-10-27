//! Expected values come from node 24 evaluating the same expressions
//! (`String(n)`, `Number(s)`, `parseInt(s)`, `a > b`, `a == b`, `String(v)`,
//! `JSON.stringify`, `JSON.parse`, `decodeURIComponent`).

use {super::*, crate::js_object, std::cmp::Ordering};

fn s(v: &str) -> JsValue {
    JsValue::from(v)
}

fn arr(items: &[&str]) -> JsValue {
    JsValue::Array(items.iter().map(|v| s(v)).collect())
}

fn n(v: f64) -> JsValue {
    JsValue::Number(v)
}

#[test]
fn number_to_string_matches_js() {
    let cases: &[(f64, &str)] = &[
        (0.0, "0"),
        (-0.0, "0"),
        (1.0, "1"),
        (-1.0, "-1"),
        (1.5, "1.5"),
        (0.1, "0.1"),
        (1e21, "1e+21"),
        (1e20, "100000000000000000000"),
        (123456789012345680000.0, "123456789012345680000"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (1.2345e-7, "1.2345e-7"),
        (5e-324, "5e-324"),
        (1.7976931348623157e308, "1.7976931348623157e+308"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (0.000001234, "0.000001234"),
        (100.0, "100"),
        (9007199254740992.0, "9007199254740992"),
        (9007199254740994.0, "9007199254740994"),
        (1.0 / 3.0, "0.3333333333333333"),
        (15000000000000000000000.0, "1.5e+22"),
        (4.35, "4.35"),
        (0.1 + 0.2, "0.30000000000000004"),
        (-1.5e-10, "-1.5e-10"),
        (123e-20, "1.23e-18"),
    ];
    for &(value, expected) in cases {
        assert_eq!(number_to_string(value), expected, "{value:e}");
    }
}

#[test]
fn string_to_number_matches_js() {
    let cases: &[(&str, f64)] = &[
        ("", 0.0),
        (" ", 0.0),
        (" 12 ", 12.0),
        ("12px", f64::NAN),
        ("0x1A", 26.0),
        ("0X1a", 26.0),
        ("-0x1A", f64::NAN),
        ("0b101", 5.0),
        ("0o17", 15.0),
        ("1e3", 1000.0),
        (".5", 0.5),
        ("5.", 5.0),
        (".", f64::NAN),
        ("+5", 5.0),
        ("-5", -5.0),
        ("Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
        ("+Infinity", f64::INFINITY),
        ("infinity", f64::NAN),
        ("NaN", f64::NAN),
        ("1_000", f64::NAN),
        ("\u{a0}12\u{feff}", 12.0),
        ("\u{85}12", f64::NAN),
        ("00012", 12.0),
        ("1e", f64::NAN),
        ("1e+", f64::NAN),
        ("0x", f64::NAN),
        ("1,2", f64::NAN),
        ("[object Object]", f64::NAN),
    ];
    for &(input, expected) in cases {
        let got = string_to_number(input);
        assert!(
            got == expected || (got.is_nan() && expected.is_nan()),
            "Number({input:?}) = {got}, want {expected}"
        );
    }
}

#[test]
fn parse_int_matches_js() {
    let cases: &[(&str, f64)] = &[
        ("", f64::NAN),
        ("12", 12.0),
        (" 12px", 12.0),
        ("-12", -12.0),
        ("+12", 12.0),
        ("0x1A", 26.0),
        ("0x", f64::NAN),
        ("1e3", 1.0),
        ("12.9", 12.0),
        ("abc", f64::NAN),
        ("  -0", -0.0),
        ("\u{a0}7", 7.0),
        ("9007199254740993", 9007199254740992.0),
        ("123456789012345678901234567890", 1.2345678901234568e29),
        ("Infinity", f64::NAN),
        ("0b11", 0.0),
        ("08", 8.0),
    ];
    for &(input, expected) in cases {
        let got = parse_int_str(input);
        assert!(
            got == expected || (got.is_nan() && expected.is_nan()),
            "parseInt({input:?}) = {got}, want {expected}"
        );
    }
    // parseInt(undefined), parseInt(null), parseInt(['12','3'])
    assert!(JsValue::Undefined.parse_int().unwrap().is_nan());
    assert!(JsValue::Null.parse_int().unwrap().is_nan());
    assert_eq!(arr(&["12", "3"]).parse_int().unwrap(), 12.0);
}

#[test]
fn parse_float_reads_the_longest_prefix() {
    assert_eq!(parse_float_str("1.50"), 1.5);
    assert_eq!(parse_float_str(" 1e+21x"), 1e21);
    assert_eq!(parse_float_str("1e"), 1.0);
    assert_eq!(parse_float_str("-Infinity"), f64::NEG_INFINITY);
    assert_eq!(parse_float_str(".5."), 0.5);
    assert!(parse_float_str("NaN").is_nan());
    assert!(parse_float_str("0x10").eq(&0.0));
}

/// The query values the routes compare (`block_height > current + 1`).
fn query_values() -> Vec<(&'static str, JsValue)> {
    vec![
        ("undefined", JsValue::Undefined),
        ("\"5\"", s("5")),
        ("\"abc\"", s("abc")),
        ("\"\"", s("")),
        ("\" 7 \"", s(" 7 ")),
        ("[\"5\"]", arr(&["5"])),
        ("[\"5\",\"6\"]", arr(&["5", "6"])),
        ("[]", arr(&[])),
        ("{\"b\":\"\"}", js_object! { "b" => "" }.into()),
        ("\"0x10\"", s("0x10")),
        ("\"1e2\"", s("1e2")),
    ]
}

#[test]
fn greater_than_matches_js() {
    let rhs: Vec<(&str, JsValue)> = vec![
        ("4", n(4.0)),
        ("5", n(5.0)),
        ("null", JsValue::Null),
        ("-1", n(-1.0)),
        ("\"4\"", s("4")),
        ("NaN", n(f64::NAN)),
    ];
    // rows: lhs; columns follow `rhs`.
    let expected: &[(&str, [bool; 6])] = &[
        ("undefined", [false, false, false, false, false, false]),
        ("\"5\"", [true, false, true, true, true, false]),
        ("\"abc\"", [false, false, false, false, true, false]),
        ("\"\"", [false, false, false, true, false, false]),
        ("\" 7 \"", [true, true, true, true, false, false]),
        ("[\"5\"]", [true, false, true, true, true, false]),
        ("[\"5\",\"6\"]", [false, false, false, false, true, false]),
        ("[]", [false, false, false, true, false, false]),
        ("{\"b\":\"\"}", [false, false, false, false, true, false]),
        ("\"0x10\"", [true, true, true, true, false, false]),
        ("\"1e2\"", [true, true, true, true, false, false]),
    ];
    for ((name, lhs), (row_name, row)) in query_values().iter().zip(expected) {
        assert_eq!(name, row_name);
        for ((rhs_name, rhs), want) in rhs.iter().zip(row) {
            assert_eq!(lhs.gt(rhs).unwrap(), *want, "{name} > {rhs_name}");
        }
    }
    // null + 1 = 1; "5" > null + 1
    let one = JsValue::Null.add(&n(1.0)).unwrap();
    assert_eq!(one, n(1.0));
    assert!(s("5").gt(&one).unwrap());
    assert!(!s("10").gt(&s("9")).unwrap());
    // UTF-16 code unit order: "￿" < "\u{10000}" is false
    assert!(!s("\u{ffff}").lt(&s("\u{10000}")).unwrap());
}

#[test]
fn ge_and_le_are_false_when_either_side_is_nan() {
    assert!(!JsValue::Undefined.ge(&n(1.0)).unwrap());
    assert!(!JsValue::Undefined.le(&n(1.0)).unwrap());
    assert!(s("2").ge(&n(2.0)).unwrap());
    assert!(s("2").le(&n(2.0)).unwrap());
}

#[test]
fn loose_equality_matches_js() {
    let mut lhs = query_values();
    lhs.push(("null", JsValue::Null));
    lhs.push(("0", n(0.0)));
    lhs.push(("[\"\"]", arr(&[""])));
    lhs.push(("[\"a\"]", arr(&["a"])));
    let rhs: Vec<(&str, JsValue)> = vec![
        ("\"\"", s("")),
        ("null", JsValue::Null),
        ("undefined", JsValue::Undefined),
        ("0", n(0.0)),
        ("\"a\"", s("a")),
    ];
    let expected: &[(&str, [bool; 5])] = &[
        ("undefined", [false, true, true, false, false]),
        ("\"5\"", [false, false, false, false, false]),
        ("\"abc\"", [false, false, false, false, false]),
        ("\"\"", [true, false, false, true, false]),
        ("\" 7 \"", [false, false, false, false, false]),
        ("[\"5\"]", [false, false, false, false, false]),
        ("[\"5\",\"6\"]", [false, false, false, false, false]),
        ("[]", [true, false, false, true, false]),
        ("{\"b\":\"\"}", [false, false, false, false, false]),
        ("\"0x10\"", [false, false, false, false, false]),
        ("\"1e2\"", [false, false, false, false, false]),
        ("null", [false, true, true, false, false]),
        ("0", [true, false, false, true, false]),
        ("[\"\"]", [true, false, false, true, false]),
        ("[\"a\"]", [false, false, false, false, true]),
    ];
    for ((name, lhs), (row_name, row)) in lhs.iter().zip(expected) {
        assert_eq!(name, row_name);
        for ((rhs_name, rhs), want) in rhs.iter().zip(row) {
            assert_eq!(lhs.loose_eq(rhs).unwrap(), *want, "{name} == {rhs_name}");
        }
    }
}

#[test]
fn to_string_matches_js() {
    let expected = [
        "undefined",
        "5",
        "abc",
        "",
        " 7 ",
        "5",
        "5,6",
        "",
        "[object Object]",
        "0x10",
        "1e2",
    ];
    for ((name, v), want) in query_values().iter().zip(expected) {
        assert_eq!(v.to_js_string().unwrap(), want, "String({name})");
    }
    assert_eq!(JsValue::Null.to_js_string().unwrap(), "null");
    assert_eq!(JsValue::Bool(true).to_js_string().unwrap(), "true");
    let nested = JsValue::Array(vec![
        JsValue::Null,
        JsValue::Undefined,
        n(1.0),
        arr(&["a", "b"]),
    ]);
    assert_eq!(nested.to_js_string().unwrap(), ",,1,a,b");
}

#[test]
fn an_own_to_string_property_makes_coercion_throw() {
    // qs (allowPrototypes) can produce {toString: '1'}; String(it), it > 1
    // and [it] + '' throw TypeError. An own valueOf is skipped.
    let own_to_string = JsValue::from(js_object! { "toString" => "1" });
    assert!(own_to_string.to_js_string().is_err());
    assert!(own_to_string.gt(&n(1.0)).is_err());
    assert!(JsValue::Array(vec![own_to_string]).add(&s("")).is_err());
    let own_value_of = JsValue::from(js_object! { "valueOf" => "1" });
    assert!(!own_value_of.gt(&n(1.0)).unwrap());
}

#[test]
fn add_concatenates_when_either_side_is_a_string() {
    assert_eq!(JsValue::Null.add(&s("")).unwrap(), s("null"));
    assert_eq!(n(7.0).add(&s("")).unwrap(), s("7"));
    assert_eq!(s("%").add(&arr(&["a", "b"])).unwrap(), s("%a,b"));
    assert_eq!(n(2.0).add(&n(3.0)).unwrap(), n(5.0));
    assert!(JsValue::Undefined.add(&n(1.0)).unwrap().eq(&n(f64::NAN)));
}

#[test]
fn truthiness_and_or() {
    for falsy in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        n(0.0),
        n(f64::NAN),
        s(""),
    ] {
        assert!(!falsy.truthy(), "{falsy:?}");
    }
    for truthy in [arr(&[]), js_object! {}.into(), s("0"), n(-1.0)] {
        assert!(truthy.truthy(), "{truthy:?}");
    }
    assert_eq!(JsValue::Undefined.or(s("")), s(""));
    assert_eq!(s("x").or(s("")), s("x"));
}

#[test]
fn case_mapping_throws_off_strings() {
    assert_eq!(s("OrDi").to_lower_case().unwrap(), "ordi");
    assert_eq!(s("ordi").to_upper_case().unwrap(), "ORDI");
    assert!(JsValue::Undefined.to_lower_case().is_err());
    assert!(arr(&["a"]).to_lower_case().is_err());
    assert!(
        JsValue::from(js_object! { "b" => "" })
            .to_upper_case()
            .is_err()
    );
}

#[test]
fn json_stringify_matches_js() {
    let text = "a\"b\\c\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}\u{2028}é😀/";
    assert_eq!(
        json_stringify(&s(text)).unwrap(),
        "\"a\\\"b\\\\c\\n\\r\\t\\b\\f\\u0001\\u001f\u{7f}\u{2028}é😀/\""
    );
    let obj = js_object! {
        "b" => 1,
        "2" => "x",
        "a" => JsValue::Undefined,
        "1" => JsValue::Array(vec![JsValue::Undefined, n(f64::NAN), n(f64::INFINITY), n(-0.0), JsValue::Null]),
        "01" => true,
    };
    assert_eq!(
        json_stringify(&obj.into()).unwrap(),
        r#"{"1":[null,null,null,0,null],"2":"x","b":1,"01":true}"#
    );
    assert_eq!(json_stringify(&JsValue::Undefined), None);
}

#[test]
fn json_parse_matches_js() {
    let parsed = json_parse(
        r#"{"2": null, "a": [1, 2.50, "x", 12345678901234567890], "b": {"c": "é"}, "1": true}"#,
    )
    .unwrap();
    assert_eq!(
        json_stringify(&parsed).unwrap(),
        r#"{"1":true,"2":null,"a":[1,2.5,"x",12345678901234567000],"b":{"c":"é"}}"#
    );
    // duplicate keys: the last value at the first key's position
    let dup = json_parse(r#"{"a": 1, "a": 2, "0": 0}"#).unwrap();
    assert_eq!(json_stringify(&dup).unwrap(), r#"{"0":0,"a":2}"#);
    assert_eq!(json_parse(r#""s""#).unwrap(), s("s"));
    assert_eq!(json_parse("1e400").unwrap(), n(f64::INFINITY));
    assert_eq!(json_parse(r#""😀\/""#).unwrap(), s("😀/"));
    for bad in [
        "",
        "{",
        "[1,]",
        "01",
        "1.",
        "\"\u{1}\"",
        "tru",
        "{\"a\" 1}",
        "1 2",
        "'a'",
    ] {
        assert!(json_parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn decode_uri_component_matches_js() {
    assert_eq!(decode_uri_component("a%20b%2Fc").as_deref(), Some("a b/c"));
    assert_eq!(decode_uri_component("%E2%9C%93").as_deref(), Some("✓"));
    assert_eq!(decode_uri_component("+").as_deref(), Some("+"));
    assert_eq!(decode_uri_component("%00").as_deref(), Some("\0"));
    for bad in [
        "%zz",
        "%",
        "%4",
        "%ED%A0%80",
        "%C0%80",
        "%E2%9C",
        "%FF",
        "%E2%9Cx",
    ] {
        assert_eq!(decode_uri_component(bad), None, "{bad}");
    }
}

#[test]
fn compare_strings_uses_utf16_code_units() {
    assert_eq!(compare_strings("a", "b"), Ordering::Less);
    assert_eq!(compare_strings("\u{ffff}", "\u{10000}"), Ordering::Greater);
    assert_eq!(compare_strings("ab", "a"), Ordering::Greater);
    assert_eq!(compare_strings("", ""), Ordering::Equal);
}

#[test]
fn sort_by_number_is_stable() {
    // rows.sort((a, b) => parseInt(b.x) - parseInt(a.x))
    let mut rows = vec![("a", "2"), ("b", "7"), ("c", "10"), ("d", "2"), ("e", "7")];
    sort_by_number(&mut rows, |a, b| parse_int_str(b.1) - parse_int_str(a.1));
    let order: Vec<_> = rows.iter().map(|r| r.0).collect();
    assert_eq!(order, ["c", "b", "e", "a", "d"]);
}

#[test]
fn sort_by_number_survives_an_inconsistent_comparator() {
    // A NaN result counts as +0, which no total order allows; the sort
    // still ends with every element in place of some element.
    let mut rows = vec!["2", "x", "10", "2", "y", "1"];
    sort_by_number(&mut rows, |a, b| parse_int_str(b) - parse_int_str(a));
    let mut sorted = rows.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, ["1", "10", "2", "2", "x", "y"]);
}

#[test]
fn js_trim_uses_js_whitespace() {
    assert_eq!(js_trim("\u{feff}\u{a0} a b \u{3000}\n"), "a b");
    assert_eq!(js_trim("\u{85}a"), "\u{85}a");
}
