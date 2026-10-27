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
fn bind_params_writes_unknown_typed_literals() {
    let sql = "select a from t where b < $1 and c = $2 and d = $1 limit $3;";
    let bound = bind_params(sql, &[Some("5".into()), Some("it's".into()), None]).unwrap();
    assert_eq!(
        bound,
        "select a from t where b < '5' and c = 'it''s' and d = '5' limit NULL;"
    );
}

#[test]
fn bind_params_escapes_backslashes_with_an_e_string() {
    let bound = bind_params("select $1", &[Some(r"a\'b".into())]).unwrap();
    assert_eq!(bound, r"select  E'a\\''b'");
}

#[test]
fn bind_params_leaves_quoted_text_and_comments_alone() {
    let sql = "select '$1', \"$1\", $$ $1 $$, $tag$ $1 $tag$, E'\\' $1', a$1, $1 -- $2\n/* $2 /* $3 */ $2 */";
    let bound = bind_params(sql, &[Some("x".into())]).unwrap();
    assert_eq!(
        bound,
        "select '$1', \"$1\", $$ $1 $$, $tag$ $1 $tag$, E'\\' $1', a$1, 'x' -- $2\n/* $2 /* $3 */ $2 */"
    );
}

#[test]
fn bind_params_needs_one_value_per_placeholder() {
    // postgres: bind message supplies 1 parameters, but prepared statement "" requires 0
    assert!(bind_params("select 1", &[Some("1".into())]).is_err());
    assert!(bind_params("select $2", &[Some("1".into())]).is_err());
    assert!(bind_params("select $1", &[]).is_err());
    assert_eq!(bind_params("select 1", &[]).unwrap(), "select 1");
}

#[test]
fn bind_params_rejects_nul() {
    // postgres rejects 0x00 in text: invalid byte sequence for encoding "UTF8"
    assert!(bind_params("select $1", &[Some("a\0b".into())]).is_err());
}

#[tokio::test]
async fn a_fake_db_sees_the_js_params() {
    let db = Db::fake(|sql, params| {
        assert_eq!(sql, "select $1");
        assert_eq!(params, [JsValue::from("800")]);
        Ok(vec![js_object! { "a" => 1 }])
    });
    let rows = db.query("select $1", &[s("800")]).await.unwrap();
    assert_eq!(rows, [js_object! { "a" => 1 }]);
}
