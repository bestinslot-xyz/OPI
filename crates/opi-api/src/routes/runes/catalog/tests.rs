//! The rune routes' parameter rules.

use {super::*, crate::qs::parse_query, utoipa::PartialSchema};

fn query(text: &str) -> JsObject {
    parse_query(Some(text))
}

#[test]
fn runes_are_named_by_id_or_name() {
    assert_eq!(
        rune_param(&query("rune=840000%3A3")),
        Ok(RuneRef::Id("840000:3".into()))
    );
    assert_eq!(
        rune_param(&query("rune=uncommon%E2%80%A2goods")),
        Ok(RuneRef::Name("UNCOMMONGOODS".into()))
    );
    for bad in [
        "",
        "rune=",
        "rune=1%3Ax",
        "rune=A..B",
        "rune=a&rune=b",
        "rune=A%00",
    ] {
        assert!(rune_param(&query(bad)).is_err(), "{bad}");
    }
    let long = format!("rune={}", "A".repeat(129));
    assert!(rune_param(&query(&long)).is_err());
}

#[test]
fn the_runes_list_reads_its_order_search_and_cursor() {
    let request = RunesRequest::from_query(&query("")).unwrap();
    assert_eq!(request.sort, RuneSort::Holders);
    assert!(!request.ascending);
    assert_eq!(request.search, None);
    assert_eq!(
        request.page,
        KeyPage {
            limit: 100,
            after: None
        }
    );
    let request = RunesRequest::from_query(&query(
        "sort=mint_progress&order=asc&search=uncommon.g&limit=5&after=-1%3A12",
    ))
    .unwrap();
    assert_eq!(request.sort, RuneSort::MintProgress);
    assert!(request.ascending);
    assert_eq!(request.search, Some(("UNCOMMONG%".into(), 0xff, 0x80)));
    assert_eq!(
        request.page.after,
        Some(RunesCursor {
            key: "-1".into(),
            number: "12".into()
        })
    );
    assert!(RunesRequest::from_query(&query("after=0.25%3A3")).is_ok());
    for bad in [
        "sort=name",
        "order=up",
        "search=a-b",
        "search=%00",
        "after=3",
        "after=x%3A3",
        "after=1%3A-3",
        "after=1.%3A3",
        "limit=0",
        "limit=501",
    ] {
        assert!(RunesRequest::from_query(&query(bad)).is_err(), "{bad}");
    }
}

#[test]
fn the_sort_schema_names_each_sort() {
    let schema = serde_json::to_value(RuneSort::schema()).unwrap();
    let names: Vec<&str> = RuneSort::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(schema["enum"], serde_json::json!(names));
}

#[test]
fn cursors_name_a_place_in_their_order() {
    assert_eq!(
        holders_cursor("500:0014AB"),
        Some(("500".into(), "0014ab".into()))
    );
    for bad in ["500", "x:00", "5:0", "5:zz", "-5:00"] {
        assert_eq!(holders_cursor(bad), None, "{bad}");
    }
    assert_eq!(number_cursor("17"), Some("17".into()));
    assert_eq!(number_cursor("1.5"), None);
}
