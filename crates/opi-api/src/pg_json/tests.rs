//! Expected values are `JSON.stringify(types.getTypeParser(oid, 'text')(text))`
//! from pg-types 2.2.0 (node-pg 8.23's parsers), run with TZ=Europe/London
//! and TZ=UTC.

use {
    super::{PgJsonError, ToJs, decode_text, oid},
    crate::js::{JsValue, json_stringify},
    jiff::tz::TimeZone,
};

fn typed(value: impl ToJs) -> String {
    json_stringify(&value.to_js()).unwrap()
}

#[test]
fn typed_values_take_their_node_pg_form() {
    // int2/int4 are numbers, int8 its decimal text, as pg-types answers
    assert_eq!(typed(7i16), "7");
    assert_eq!(typed(-5i32), "-5");
    assert_eq!(typed(i64::MAX), r#""9223372036854775807""#);
    assert_eq!(typed(Some(328239i32)), "328239");
    assert_eq!(typed(None::<i32>), "null");
    assert_eq!(typed(true), "true");
    assert_eq!(typed("t".to_string()), r#""t""#);
    assert_eq!(typed(vec![Some("a".to_string()), None]), r#"["a",null]"#);
    assert_eq!(typed(vec![1i64, -2]), r#"["1","-2"]"#);
    assert_eq!(typed(vec![1i32, -2]), "[1,-2]");
    // float8 is exact; float4 is parseFloat of Postgres's shortest float4
    // text (0.1::float4 prints "0.1", not 0.10000000149011612)
    assert_eq!(typed(1.5f64), "1.5");
    assert_eq!(typed(0.1f32), "0.1");
    assert_eq!(typed(16777217f32), "16777216");
    assert_eq!(typed(1e21f32), "1e+21");
    assert_eq!(typed(f32::NAN), "null");
    assert_eq!(typed(f32::INFINITY), "null");
    assert_eq!(
        f32::NEG_INFINITY.to_js(),
        JsValue::Number(f64::NEG_INFINITY)
    );
}

#[test]
fn text_casts_decode_with_the_text_rules() {
    // numeric, jsonb and timestamps arrive as `::text` columns
    let tz = TimeZone::UTC;
    assert_eq!(
        json_stringify(&decode_text(oid::NUMERIC, Some("1.50"), &tz).unwrap()).unwrap(),
        r#""1.50""#
    );
    assert_eq!(
        json_stringify(&super::text(oid::JSONB, Some(r#"{"b": 1, "a": 2}"#)).unwrap()).unwrap(),
        r#"{"b":1,"a":2}"#
    );
    assert_eq!(
        super::text(oid::NUMERIC_ARRAY, None).unwrap(),
        JsValue::Null
    );
}

#[test]
fn date_is_a_local_midnight() {
    let cases: &[(&str, &str, &str)] = &[
        // text, TZ=Europe/London, TZ=UTC
        (
            "2024-07-02",
            r#""2024-07-01T23:00:00.000Z""#,
            r#""2024-07-02T00:00:00.000Z""#,
        ),
        (
            "2024-01-02",
            r#""2024-01-02T00:00:00.000Z""#,
            r#""2024-01-02T00:00:00.000Z""#,
        ),
        // London's local mean time before 1847 is -00:01:15
        (
            "0099-01-02",
            r#""0099-01-02T00:01:15.000Z""#,
            r#""0099-01-02T00:00:00.000Z""#,
        ),
        (
            "0044-03-15 BC",
            r#""-000043-03-15T00:01:15.000Z""#,
            r#""-000043-03-15T00:00:00.000Z""#,
        ),
        (
            "12345-01-01",
            r#""+012345-01-01T00:00:00.000Z""#,
            r#""+012345-01-01T00:00:00.000Z""#,
        ),
        ("infinity", "null", "null"),
        ("-infinity", "null", "null"),
    ];
    for &(text, in_london, in_utc) in cases {
        assert_eq!(json(oid::DATE, text, &london()), in_london, "{text:?}");
        assert_eq!(json(oid::DATE, text, &TimeZone::UTC), in_utc, "{text:?}");
    }
    assert_eq!(
        json(1182, "{2024-07-02,NULL}", &london()),
        r#"["2024-07-01T23:00:00.000Z",null]"#
    );
}

fn london() -> TimeZone {
    TimeZone::get("Europe/London").unwrap()
}

fn json(oid: u32, text: &str, tz: &TimeZone) -> String {
    json_stringify(&decode_text(oid, Some(text), tz).unwrap()).unwrap()
}

#[test]
fn the_route_inventory_types_decode_like_node_pg() {
    let tz = TimeZone::UTC;
    let cases: &[(u32, &str, &str)] = &[
        // int8 and numeric stay strings, int4 becomes a number
        (20, "123", r#""123""#),
        (20, "-123", r#""-123""#),
        (1700, "1.50", r#""1.50""#),
        (1700, "NaN", r#""NaN""#),
        (1700, "-0.000", r#""-0.000""#),
        (
            1700,
            "15000000000000000000000",
            r#""15000000000000000000000""#,
        ),
        (23, "-5", "-5"),
        (23, "328239", "328239"),
        (25, "t", r#""t""#),
        (1043, "v", r#""v""#),
        (
            3802,
            r#"{"2": null, "a": [1, 2.50, "x", 12345678901234567890], "b": {"c": "é"}, "1": true}"#,
            r#"{"1":true,"2":null,"a":[1,2.5,"x",12345678901234567000],"b":{"c":"é"}}"#,
        ),
        (
            1009,
            r#"{a,"b c","d\"e",NULL,"NULL",""}"#,
            r#"["a","b c","d\"e",null,"NULL",""]"#,
        ),
        (1009, "{}", "[]"),
        (1009, "{{a,b},{c,d}}", r#"[["a","b"],["c","d"]]"#),
        (1009, "[0:1]={x,y}", r#"["x","y"]"#),
        (
            1009,
            "{188728:6,188798:365}",
            r#"["188728:6","188798:365"]"#,
        ),
        // numeric[] goes through parseFloat
        (1231, "{1000,10,NULL,1.50,NaN}", "[1000,10,null,1.5,null]"),
        (
            1231,
            "{12345678901234567890123}",
            "[1.2345678901234568e+22]",
        ),
    ];
    for &(oid, text, expected) in cases {
        assert_eq!(json(oid, text, &tz), expected, "{oid} {text:?}");
    }
}

#[test]
fn the_other_node_pg_types_decode_like_node_pg() {
    let tz = TimeZone::UTC;
    let cases: &[(u32, &str, &str)] = &[
        (21, "7", "7"),
        (26, "4294967295", "4294967295"),
        (701, "1.5", "1.5"),
        (701, "NaN", "null"),
        (701, "Infinity", "null"),
        (701, "1e+21", "1e+21"),
        (700, "0.1", "0.1"),
        (701, "-0", "0"),
        (16, "t", "true"),
        (16, "f", "false"),
        (114, r#"{"a": 1, "a": 2, "0": 0}"#, r#"{"0":0,"a":2}"#),
        (114, r#""s""#, r#""s""#),
        (114, "1e400", "null"),
        (1042, "c  ", r#""c  ""#),
        (19, "n", r#""n""#),
        (1016, "{1,-2,NULL}", r#"["1","-2",null]"#),
        (1007, "{1,-2,NULL}", "[1,-2,null]"),
        (1005, "{3}", "[3]"),
        (1000, "{t,f,NULL}", "[true,false,null]"),
        (1022, "{1.5,Infinity}", "[1.5,null]"),
        (3807, r#"{"{\"a\": 1}",NULL}"#, r#"[{"a":1},null]"#),
        (1015, "{v}", r#"["v"]"#),
    ];
    for &(oid, text, expected) in cases {
        assert_eq!(json(oid, text, &tz), expected, "{oid} {text:?}");
    }
}

#[test]
fn timestamptz_becomes_a_js_iso_string() {
    let tz = london();
    let cases: &[(&str, &str)] = &[
        (
            "2024-01-02 03:04:05.678+00",
            r#""2024-01-02T03:04:05.678Z""#,
        ),
        (
            "2024-01-02 03:04:05.6789+05:30",
            r#""2024-01-01T21:34:05.678Z""#,
        ),
        ("2024-01-02 03:04:05-08", r#""2024-01-02T11:04:05.000Z""#),
        ("infinity", "null"),
        ("-infinity", "null"),
        ("0099-01-02 03:04:05+00", r#""0099-01-02T03:04:05.000Z""#),
        (
            "0044-03-15 12:00:00+00 BC",
            r#""-000043-03-15T12:00:00.000Z""#,
        ),
        (
            "12345-01-01 00:00:00+00",
            r#""+012345-01-01T00:00:00.000Z""#,
        ),
        (
            "2024-06-30 23:59:59.9999995+00",
            r#""2024-06-30T23:59:59.999Z""#,
        ),
    ];
    for &(text, expected) in cases {
        assert_eq!(json(1184, text, &tz), expected, "{text:?}");
        assert_eq!(json(1184, text, &TimeZone::UTC), expected, "{text:?}");
    }
    assert_eq!(
        json(1185, r#"{"2024-01-02 03:04:05+00",NULL}"#, &tz),
        r#"["2024-01-02T03:04:05.000Z",null]"#
    );
}

#[test]
fn timestamp_reads_as_local_time() {
    let cases: &[(&str, &str, &str)] = &[
        // text, TZ=Europe/London, TZ=UTC
        (
            "2024-01-02 03:04:05.678",
            r#""2024-01-02T03:04:05.678Z""#,
            r#""2024-01-02T03:04:05.678Z""#,
        ),
        (
            "2024-07-02 03:04:05",
            r#""2024-07-02T02:04:05.000Z""#,
            r#""2024-07-02T03:04:05.000Z""#,
        ),
        // a time the spring-forward gap skips
        (
            "2024-03-31 01:30:00",
            r#""2024-03-31T01:30:00.000Z""#,
            r#""2024-03-31T01:30:00.000Z""#,
        ),
        // a time the autumn fold repeats: the earlier one
        (
            "2024-10-27 01:30:00",
            r#""2024-10-27T00:30:00.000Z""#,
            r#""2024-10-27T01:30:00.000Z""#,
        ),
    ];
    for &(text, in_london, in_utc) in cases {
        assert_eq!(json(1114, text, &london()), in_london, "{text:?}");
        assert_eq!(json(1114, text, &TimeZone::UTC), in_utc, "{text:?}");
    }
}

#[test]
fn null_is_null_for_every_type() {
    assert_eq!(
        json_stringify(&decode_text(23, None, &TimeZone::UTC).unwrap()).unwrap(),
        "null"
    );
    assert_eq!(
        json_stringify(&decode_text(99999, None, &TimeZone::UTC).unwrap()).unwrap(),
        "null"
    );
}

#[test]
fn an_unknown_type_is_an_error() {
    // uuid (2950), name[] (1003) and custom types have no rule.
    for oid in [2950, 1003, 99999] {
        match decode_text(oid, Some("x"), &TimeZone::UTC) {
            Err(PgJsonError::UnsupportedType(got)) => assert_eq!(got, oid),
            other => panic!("{oid}: {other:?}"),
        }
    }
}

#[test]
fn malformed_json_is_an_error() {
    assert!(decode_text(3802, Some("{"), &TimeZone::UTC).is_err());
}
