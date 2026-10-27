//! Expected values are `JSON.stringify(qs.parse(s, { allowPrototypes: true }))`
//! from qs 6.11.0, the parser Express 4.18.2 uses for `request.query`.

use {
    super::parse_query,
    crate::js::{JsValue, json_stringify},
};

fn parsed(raw: &str) -> String {
    json_stringify(&JsValue::Object(parse_query(Some(raw)))).unwrap()
}

#[test]
fn matches_qs() {
    let cases: &[(&str, &str)] = &[
        ("", r#"{}"#),
        ("a=1", r#"{"a":"1"}"#),
        ("a=1&a=2", r#"{"a":["1","2"]}"#),
        ("a=1&a=2&a=3", r#"{"a":["1","2","3"]}"#),
        ("a[]=1", r#"{"a":["1"]}"#),
        ("a[]=1&a[]=2", r#"{"a":["1","2"]}"#),
        ("a[b]=", r#"{"a":{"b":""}}"#),
        ("a[b]=c&a[d]=e", r#"{"a":{"b":"c","d":"e"}}"#),
        ("a[0]=x&a[1]=y", r#"{"a":["x","y"]}"#),
        ("a[1]=x", r#"{"a":["x"]}"#),
        ("a[21]=x", r#"{"a":{"21":"x"}}"#),
        ("a[20]=x", r#"{"a":["x"]}"#),
        (
            "a[b][c][d][e][f][g][h]=1",
            r#"{"a":{"b":{"c":{"d":{"e":{"f":{"[g][h]":"1"}}}}}}}"#,
        ),
        (
            "a[b][c][d][e][f]=1",
            r#"{"a":{"b":{"c":{"d":{"e":{"f":"1"}}}}}}"#,
        ),
        ("a=1&a[b]=2", r#"{"a":["1",{"b":"2"}]}"#),
        ("a[b]=1&a=2", r#"{"a":{"2":true,"b":"1"}}"#),
        ("a[b]=1&a=", r#"{"a":{"b":"1"}}"#),
        ("a", r#"{"a":""}"#),
        ("a&b=", r#"{"a":"","b":""}"#),
        ("=x", r#"{}"#),
        ("&&a=1&", r#"{"a":"1"}"#),
        ("a=%zz", r#"{"a":"%zz"}"#),
        ("a=%E2%9C%93", r#"{"a":"✓"}"#),
        ("a=%ED%A0%80", r#"{"a":"%ED%A0%80"}"#),
        ("a=%C0%80", r#"{"a":"%C0%80"}"#),
        ("a=b+c%20d", r#"{"a":"b c d"}"#),
        ("a+b=c", r#"{"a b":"c"}"#),
        ("a[=1", r#"{"a[":"1"}"#),
        ("a]=1", r#"{"a]":"1"}"#),
        ("a[b[c]]=1", r#"{"a[b":{"c":"1"}}"#),
        ("[a]=1", r#"{"a":"1"}"#),
        ("[]=1", r#"{"0":"1"}"#),
        ("a[__proto__]=1", r#"{"a":{}}"#),
        ("__proto__=1", r#"{}"#),
        ("toString=1", r#"{"toString":"1"}"#),
        ("a[0]=1&a[b]=2", r#"{"a":{"0":"1","b":"2"}}"#),
        ("a[]=1&a[b]=2", r#"{"a":{"0":"1","b":"2"}}"#),
        ("a[0][b]=1&a[0][c]=2", r#"{"a":[{"b":"1","c":"2"}]}"#),
        ("a[0]=1&a[0]=2", r#"{"a":[["1","2"]]}"#),
        ("b=1&0=2&a=3", r#"{"0":"2","b":"1","a":"3"}"#),
        ("a[1]=x&a[0]=y", r#"{"a":["y","x"]}"#),
        ("a=1&a[]=2", r#"{"a":["1","2"]}"#),
        ("a[]=1&a=2", r#"{"a":["1","2"]}"#),
        ("a[b]=1&a[b]=2", r#"{"a":{"b":["1","2"]}}"#),
        ("a[%62]=1", r#"{"a":{"b":"1"}}"#),
        ("a%5Bb%5D=1", r#"{"a":{"b":"1"}}"#),
        ("a=1=2", r#"{"a":"1=2"}"#),
        ("a[b]=c=d", r#"{"a":{"b":"c=d"}}"#),
        ("a[]=", r#"{"a":[""]}"#),
        ("a[00]=1", r#"{"a":{"00":"1"}}"#),
        ("a[-1]=1", r#"{"a":{"-1":"1"}}"#),
        ("a[ 1]=1", r#"{"a":{" 1":"1"}}"#),
        ("x=%00", r#"{"x":"\u0000"}"#),
        ("a[0]=1&a=2", r#"{"a":["1","2"]}"#),
        ("a=x&a[0]=y", r#"{"a":["x","y"]}"#),
        ("a[2]=1&a[]=2", r#"{"a":["2","1"]}"#),
        ("a[]=1&a[2]=2", r#"{"a":["1","2"]}"#),
        ("a[b]=1&a[]=2", r#"{"a":{"0":"2","b":"1"}}"#),
    ];
    for &(raw, expected) in cases {
        assert_eq!(parsed(raw), expected, "{raw:?}");
    }
}

#[test]
fn no_query_string_is_an_empty_object() {
    assert!(parse_query(None).is_empty());
}

#[test]
fn reads_at_most_1000_parameters() {
    let raw: Vec<String> = (0..1005).map(|i| format!("k{i}={i}")).collect();
    let query = parse_query(Some(&raw.join("&")));
    assert_eq!(query.len(), 1000);
    assert!(query.get("k1000").is_undefined());
}

#[test]
fn route_reads_see_js_values() {
    // request.query.block_height and friends
    let query = parse_query(Some("block_height=5&ticker=ORDI&pkscript=a&pkscript=b"));
    assert_eq!(query.get("block_height"), &JsValue::from("5"));
    assert!(query.get("address").is_undefined());
    assert_eq!(
        query.get("pkscript"),
        &JsValue::Array(vec!["a".into(), "b".into()])
    );
}
