use {super::*, crate::qs::parse_query};

const TXID: &str = "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

#[test]
fn outpoints_are_lowercased_with_a_canonical_vout() {
    let lower = TXID.to_ascii_lowercase();
    assert_eq!(
        parse_outpoint(&format!("{TXID}:0")),
        Some(format!("{lower}:0"))
    );
    assert_eq!(
        parse_outpoint(&format!("{TXID}:007")),
        Some(format!("{lower}:7"))
    );
    assert_eq!(
        parse_outpoint(&format!("{TXID}:4294967295")),
        Some(format!("{lower}:4294967295"))
    );
    for bad in [
        format!("{TXID}:4294967296"),
        format!("{TXID}:"),
        format!("{TXID}:-1"),
        format!("{TXID}:+1"),
        format!("{TXID}:1:2"),
        format!("{TXID}0:1"),
        format!("{}:1", &TXID[1..]),
        format!("{}g:1", &TXID[1..]),
        format!("{TXID}i0"),
        String::new(),
    ] {
        assert_eq!(parse_outpoint(&bad), None, "{bad}");
    }
}

#[test]
fn inscription_ids_are_lowercased_with_a_canonical_index() {
    let lower = TXID.to_ascii_lowercase();
    assert_eq!(
        parse_inscription_id(&format!("{TXID}i0")),
        Some(format!("{lower}i0"))
    );
    assert_eq!(
        parse_inscription_id(&format!("{TXID}i012")),
        Some(format!("{lower}i12"))
    );
    for bad in [
        format!("{TXID}i"),
        format!("{TXID}:0"),
        format!("{TXID}I0"),
        format!("{TXID}i4294967296"),
        format!("{}i0", &TXID[1..]),
        "é".repeat(40),
    ] {
        assert_eq!(parse_inscription_id(&bad), None, "{bad}");
    }
}

fn reference(query: &str) -> Result<InscriptionRef, BadParam> {
    InscriptionRef::from_query(&parse_query(Some(query)))
}

#[test]
fn an_inscription_is_named_by_one_of_id_and_number() {
    assert_eq!(reference("number=-5"), Ok(InscriptionRef::Number(-5)));
    assert_eq!(
        reference("number=2147483647"),
        Ok(InscriptionRef::Number(2_147_483_647))
    );
    let number = "number must be an integer from -2147483648 to 2147483647";
    for bad in [
        "number=2147483648",
        "number=-",
        "number=1.5",
        "number=%2B1",
        "number=--1",
    ] {
        assert_eq!(reference(bad), Err(BadParam(number.into())), "{bad}");
    }
    assert_eq!(
        reference(""),
        Err(BadParam("id or number is required".into()))
    );
    assert_eq!(
        reference(&format!("id={TXID}i0&number=1")),
        Err(BadParam("give id or number, not both".into()))
    );
    assert_eq!(
        reference("id=a&id=b"),
        Err(BadParam("id must be a single value".into()))
    );
}

#[test]
fn a_content_type_is_a_media_type_or_a_prefix() {
    assert_eq!(
        ContentTypeFilter::parse("Text/Plain;charset=utf-8"),
        Ok(ContentTypeFilter::Exact("text/plain".into()))
    );
    assert_eq!(
        ContentTypeFilter::parse("IMAGE/"),
        Ok(ContentTypeFilter::Prefix("image/".into()))
    );
    for bad in [";charset=utf-8", " ", "text /plain"] {
        assert!(ContentTypeFilter::parse(bad).is_err(), "{bad}");
    }
}

fn wallet(query: &str) -> Result<WalletRequest, BadParam> {
    WalletRequest::from_query(&parse_query(Some(query)))
}

#[test]
fn wallet_filters_are_checked() {
    let request = wallet(
        "pkscript=00AB&content_type=image/&from_height=5&to_height=9&cursed=false&after=12&limit=3",
    )
    .unwrap();
    assert_eq!(
        request,
        WalletRequest {
            wallet: Wallet::PkScript("00ab".into()),
            content_type: Some(ContentTypeFilter::Prefix("image/".into())),
            from_height: Some(5),
            to_height: Some(9),
            cursed: Some(false),
            limit: 3,
            after: Some(12),
        }
    );
    for (query, reason) in [
        ("address=a&cursed=yes", "cursed must be true or false"),
        (
            "address=a&from_height=9&to_height=5",
            "from_height must not be above to_height",
        ),
        (
            "address=a&to_height=-1",
            "to_height must be an integer from 0 to 2147483647",
        ),
        (
            "address=a&after=x",
            "after must be the next_after of a previous page",
        ),
        (
            "address=a&after=1234567890123456789",
            "after must be the next_after of a previous page",
        ),
        (
            "address=a&limit=501",
            "limit must be an integer from 1 to 500",
        ),
    ] {
        assert_eq!(wallet(query), Err(BadParam(reason.into())), "{query}");
    }
}

#[test]
fn a_batch_body_names_1_to_500_outpoints() {
    let outpoint = format!("{}:1", TXID.to_ascii_lowercase());
    let body = |list: &[&str]| json!({ "outpoints": list }).to_string();
    assert_eq!(
        outpoints_body(body(&[&format!("{TXID}:01"), &outpoint]).as_bytes()),
        Ok(vec![outpoint.clone(), outpoint.clone()])
    );
    let shape =
        || BadParam("the body must be JSON {\"outpoints\":[...]} with 1 to 500 outpoints".into());
    let many = vec![outpoint.as_str(); 501];
    for bad in [
        String::new(),
        "[]".into(),
        "{\"outpoints\":\"x\"}".into(),
        body(&[]),
        body(&many),
    ] {
        assert_eq!(outpoints_body(bad.as_bytes()), Err(shape()), "{bad}");
    }
    assert_eq!(
        outpoints_body(body(&[&outpoint, "x:1"]).as_bytes()),
        Err(BadParam(format!(
            "outpoints[1] must be an outpoint {OUTPOINT_FORM}"
        )))
    );
    assert_eq!(
        outpoints_body(json!({"outpoints": [outpoint, 1]}).to_string().as_bytes()),
        Err(BadParam(format!(
            "outpoints[1] must be an outpoint {OUTPOINT_FORM}"
        )))
    );
    assert_eq!(
        outpoints_body(body(&vec![outpoint.as_str(); 500]).as_bytes()).map(|list| list.len()),
        Ok(500)
    );
}
