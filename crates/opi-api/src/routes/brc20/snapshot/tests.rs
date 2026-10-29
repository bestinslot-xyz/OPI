//! The snapshot route's parameters, CSV quoting and attachment name.

use {super::*, crate::qs::parse_query};

fn request(text: &str) -> Result<SnapshotRequest, BadParam> {
    SnapshotRequest::from_query(&parse_query(Some(text)))
}

fn bad(text: &str) -> Result<SnapshotRequest, BadParam> {
    Err(BadParam(text.into()))
}

#[test]
fn the_height_defaults_to_the_tip_and_the_format_to_json() {
    assert_eq!(
        request("ticker=OrDi"),
        Ok(SnapshotRequest {
            tick: "ordi".into(),
            block_height: None,
            format: Format::Json,
        })
    );
    assert_eq!(
        request("ticker=ordi&block_height=2147483647&format=csv"),
        Ok(SnapshotRequest {
            tick: "ordi".into(),
            block_height: Some(i32::MAX),
            format: Format::Csv,
        })
    );
    assert_eq!(
        request("ticker=ordi&block_height=0&format=json").map(|r| r.block_height),
        Ok(Some(0))
    );
}

#[test]
fn invalid_parameters_are_refused() {
    let height = "block_height must be an integer from 0 to 2147483647";
    let long = format!("ticker={}", "a".repeat(257));
    for (text, expected) in [
        ("", bad("ticker is required")),
        (
            "ticker=a%00",
            bad("ticker must be at most 256 bytes without NUL"),
        ),
        (
            long.as_str(),
            bad("ticker must be at most 256 bytes without NUL"),
        ),
        ("ticker=ordi&block_height=", bad(height)),
        ("ticker=ordi&block_height=-1", bad(height)),
        ("ticker=ordi&block_height=2147483648", bad(height)),
        ("ticker=ordi&block_height=1e3", bad(height)),
        (
            "ticker=ordi&block_height=1&block_height=2",
            bad("block_height must be a single value"),
        ),
        ("ticker=ordi&format=CSV", bad("format must be json or csv")),
        ("ticker=ordi&format=xml", bad("format must be json or csv")),
        (
            "ticker=ordi&format[a]=csv",
            bad("format must be a single value"),
        ),
    ] {
        assert_eq!(request(text), expected, "{text}");
    }
}

#[test]
fn csv_fields_are_quoted_as_rfc_4180_says() {
    let mut out = String::new();
    push_csv_line(
        &mut out,
        ["plain", "", "a,b", "say \"hi\"", "two\nlines", "cr\r"],
    );
    assert_eq!(
        out,
        "plain,,\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\",\"cr\r\"\r\n"
    );
    let mut header = String::new();
    push_csv_line(&mut header, CSV_COLUMNS);
    assert_eq!(
        header,
        "address,evm_address,pkscript,brc20_balance,brc20_available_balance,prog_balance,total_balance\r\n"
    );
}

#[test]
fn attachment_names_stay_printable_ascii() {
    assert_eq!(
        content_disposition("ordi-840000.csv"),
        "attachment; filename=\"ordi-840000.csv\""
    );
    assert_eq!(
        content_disposition("a\"b\\c-1.csv"),
        "attachment; filename=\"a_b_c-1.csv\"; filename*=UTF-8''a%22b%5Cc-1.csv"
    );
    assert_eq!(
        content_disposition("äb\n-1.csv"),
        "attachment; filename=\"_b_-1.csv\"; filename*=UTF-8''%C3%A4b%0A-1.csv"
    );
}

#[test]
fn the_cap_is_four_and_at_most_half_the_pool() {
    for (max_connections, expected) in [
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 2),
        (7, 3),
        (8, 4),
        (10, 4),
        (100, 4),
    ] {
        assert_eq!(cap(max_connections), expected, "{max_connections}");
    }
}

/// The text of `source` from the merge CTE to the end of its ORDER BY.
fn merge_and_order(source: &str) -> &str {
    let start = source.find("m as (").expect("the merge CTE");
    let end = start
        + source[start..]
            .find("evm_address asc nulls first")
            .expect("the ORDER BY");
    &source[start..end]
}

#[test]
fn a_snapshot_is_settled_from_the_deepest_rollback_down() {
    // The route's documentation names the depth.
    assert_eq!(MAX_ROLLBACK_DEPTH, 1000);
    for (block_height, tip, expected) in [
        (300, 1300, true),
        (301, 1300, false),
        (0, 1000, true),
        (0, 999, false),
        (1300, 1300, false),
        (0, i32::MAX, true),
    ] {
        assert_eq!(settled(block_height, tip), expected, "{block_height} {tip}");
    }
}

#[test]
fn the_snapshot_merges_and_orders_as_the_holders_route_does() {
    let holders = merge_and_order(include_str!("../tickers.rs"));
    let snapshot = merge_and_order(include_str!("../snapshot.rs"));
    assert!(holders.contains("full join p"), "{holders}");
    assert_eq!(snapshot, holders);
}
