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
