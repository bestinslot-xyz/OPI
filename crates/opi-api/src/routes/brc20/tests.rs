//! The brc20 routes' response shaping. Expected values are node's output
//! for the same lines of api/routes/brc20.js.

use {
    super::*,
    crate::{js::json_stringify, qs::parse_query},
};

fn balance(pkscript: &str, tick: &str, overall: &str, available: &str) -> BalanceRow {
    BalanceRow {
        pkscript: pkscript.into(),
        tick: tick.into(),
        overall_balance: overall.into(),
        available_balance: available.into(),
    }
}

#[test]
fn the_current_balances_hash_is_nodes() {
    // node: the route's sort, filter, join and sha256 on these rows
    let rows = vec![
        balance("b", "x", "10", "5"),
        balance("a", "z", "0", "0"),
        balance("a", "y", "3", "3"),
        balance("\u{e000}", "t", "1", "1"),
        // UTF-16 order puts the surrogate pair before U+E000
        balance("\u{1F600}", "t", "2", "1"),
        balance("a", "\u{e9}", "00012", "7"),
        // parseInt("NaN") != 0 keeps the row; parseInt("-0") is -0, which
        // is 0
        balance("c", "q", "NaN", "0"),
        balance("d", "q", "-0", "0"),
    ];
    assert_eq!(
        current_balances_hash(rows).unwrap(),
        "0515300901cc9179c6c8572229bc6d047b4a90d648d6f60633c6541bdd23c26c"
    );
    // no rows, or only zero balances: the hash of ''
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(current_balances_hash(vec![]).unwrap(), empty);
    assert_eq!(
        current_balances_hash(vec![balance("a", "b", "0", "5")]).unwrap(),
        empty
    );
}

#[test]
fn holders_sort_by_parsed_balance_and_drop_zeros() {
    let rows: Vec<JsObject> = [
        ("a", "5"),
        ("b", "0"),
        ("c", "12"),
        // parseInt gives the same double for both, so they keep their order
        ("d", "123456789012345678901234"),
        ("e", "123456789012345678901235"),
        ("f", "5"),
        ("g", "NaN"),
        ("h", "-3"),
    ]
    .into_iter()
    .map(|(p, balance)| js_object! { "p" => p, "overall_balance" => balance })
    .collect();
    let order: String = order_holders(rows)
        .unwrap()
        .iter()
        .map(|row| row.get("p").as_str().unwrap().to_string())
        .collect();
    // node: "decafgh"
    assert_eq!(order, "decafgh");
}

#[test]
fn event_fields_are_set_as_assignments_do() {
    let mut event = pg_json::text(oid::JSONB, Some(r#"{"tick": "x", "event_type": 9}"#)).unwrap();
    add_event_fields(&mut event, "mint-inscribe".into(), "abci0".into()).unwrap();
    // an existing key keeps its place
    assert_eq!(
        json_stringify(&event).unwrap(),
        r#"{"tick":"x","event_type":"mint-inscribe","inscription_id":"abci0"}"#
    );
    // an unknown event type is undefined, which JSON.stringify leaves out
    let mut event = pg_json::text(oid::JSONB, Some("{}")).unwrap();
    add_event_fields(&mut event, JsValue::Undefined, "abci0".into()).unwrap();
    assert_eq!(
        json_stringify(&event).unwrap(),
        r#"{"inscription_id":"abci0"}"#
    );
    // an array or a primitive is left as it is
    for (text, expected) in [("[1, 2]", "[1,2]"), (r#""s""#, r#""s""#), ("5", "5")] {
        let mut event = pg_json::text(oid::JSONB, Some(text)).unwrap();
        add_event_fields(&mut event, "x".into(), "y".into()).unwrap();
        assert_eq!(json_stringify(&event).unwrap(), expected);
    }
    // null throws, as `null.event_type = ...` does
    let mut event = pg_json::text(oid::JSONB, Some("null")).unwrap();
    let err = add_event_fields(&mut event, "x".into(), "y".into()).unwrap_err();
    assert_eq!(err.kind, "TypeError");
}

#[test]
fn event_entries_name_their_type() {
    let names = js_object! { "0" => "deploy-inscribe", "1" => "mint-inscribe" };
    let entry = event_entry(&names, Some(r#"{"a": 1}"#), 1, "ii0".into()).unwrap();
    assert_eq!(
        json_stringify(&entry).unwrap(),
        r#"{"a":1,"event_type":"mint-inscribe","inscription_id":"ii0"}"#
    );
    let entry = event_entry(&names, Some(r#"{"a": 1}"#), 7, "ii0".into()).unwrap();
    assert_eq!(
        json_stringify(&entry).unwrap(),
        r#"{"a":1,"inscription_id":"ii0"}"#
    );
}

#[test]
fn the_tokens_page_follows_the_routes_coercions() {
    let page = |query: &str| explorer_tokens_page(&parse_query(Some(query)));
    // (q, offset, ascending); node evaluated the same lines
    for (query, q, offset, ascending) in [
        ("", "", 0.0, false),
        ("q=ord&items_count=50&order=asc", "ord", 50.0, true),
        ("order=ASC", "", 0.0, true),
        ("order=up&items_count=abc", "", 0.0, false),
        ("items_count=12abc", "", 12.0, false),
        ("items_count=-5", "", -5.0, false),
        ("items_count=1e21", "", 1.0, false),
        ("items_count=-0", "", 0.0, false),
        ("items_count=", "", 0.0, false),
        // a repeated key is an array: typeof is not 'string', and
        // parseInt reads its join
        ("q=a&q=b&items_count=3&items_count=4", "", 3.0, false),
        ("order=asc&order=asc", "", 0.0, false),
        ("order[a]=asc", "", 0.0, false),
    ] {
        let (got_q, got_offset, got_ascending) = page(query).unwrap();
        assert_eq!(
            (got_q.as_str(), got_offset, got_ascending),
            (q, offset, ascending),
            "{query}"
        );
    }
    // String(x) on an object with its own toString throws
    assert!(page("order[toString]=x").is_err());
}
