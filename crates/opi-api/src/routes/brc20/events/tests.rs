//! The event feeds' parameter rules.

use {super::*, crate::qs::parse_query};

fn query(text: &str) -> JsObject {
    parse_query(Some(text))
}

fn bad<T>(text: &str) -> Result<T, BadParam> {
    Err(BadParam(text.into()))
}

#[test]
fn feeds_default_and_cap() {
    assert_eq!(
        FeedRequest::from_query(&query("")),
        Ok(FeedRequest {
            limit: DEFAULT_LIMIT,
            before_id: None,
            event_type: None,
        })
    );
    assert_eq!(
        FeedRequest::from_query(&query("limit=500&before_id=42&type=mint-inscribe")),
        Ok(FeedRequest {
            limit: 500,
            before_id: Some(42),
            event_type: Some("mint-inscribe".into()),
        })
    );
    // an empty type is no filter
    assert_eq!(
        FeedRequest::from_query(&query("type=")).unwrap().event_type,
        None
    );
    let limit = "limit must be an integer from 1 to 500";
    let before = "before_id must be an integer from 1 to 999999999999999999";
    for (text, expected) in [
        ("limit=0", bad(limit)),
        ("limit=501", bad(limit)),
        ("limit=x", bad(limit)),
        ("before_id=0", bad(before)),
        ("before_id=-1", bad(before)),
        ("before_id=", bad(before)),
        ("before_id=1.5", bad(before)),
        ("before_id=1000000000000000000", bad(before)),
        (
            "before_id=1&before_id=2",
            bad("before_id must be a single value"),
        ),
        ("type[a]=x", bad("type must be a single value")),
    ] {
        assert_eq!(FeedRequest::from_query(&query(text)), expected, "{text}");
    }
}

#[test]
fn a_wallet_is_an_address_or_a_pkscript() {
    assert_eq!(
        Wallet::from_query(&query("address=tb1qalice")),
        Ok(Wallet::Address("tb1qalice".into()))
    );
    assert_eq!(
        Wallet::from_query(&query("pkscript=0014A1b2")),
        Ok(Wallet::PkScript("0014a1b2".into()))
    );
    let long_address = format!("address={}", "a".repeat(129));
    let long_pkscript = format!("pkscript={}", "a".repeat(20_002));
    let pkscript = "pkscript must be hex of at most 20000 digits";
    for (text, expected) in [
        ("", bad("address or pkscript is required")),
        ("address=", bad("address or pkscript is required")),
        ("pkscript=&address=", bad("address or pkscript is required")),
        (
            "address=a&pkscript=00",
            bad("give address or pkscript, not both"),
        ),
        ("address=a&address=b", bad("address must be a single value")),
        ("pkscript=0g", bad(pkscript)),
        ("pkscript=abc", bad(pkscript)),
        (
            long_address.as_str(),
            bad("address must be at most 128 characters"),
        ),
        (long_pkscript.as_str(), bad(pkscript)),
    ] {
        assert_eq!(Wallet::from_query(&query(text)), expected, "{text}");
    }
}

#[test]
fn the_queries_spell_the_parties_as_the_indexes_do() {
    const PARTIES: &str = r#"array_remove(ARRAY[nullif(("event" ->> 'deployer_pkScript'), ''), nullif(("event" ->> 'minted_pkScript'), ''), nullif(("event" ->> 'source_pkScript'), ''), nullif(("event" ->> 'spent_pkScript'), ''), nullif(("event" ->> 'predeployer_pkScript'), ''), nullif(("event" ->> 'deployer_wallet'), ''), nullif(("event" ->> 'minted_wallet'), ''), nullif(("event" ->> 'source_wallet'), ''), nullif(("event" ->> 'spent_wallet'), ''), nullif(("event" ->> 'predeployer_wallet'), '')], NULL)"#;
    let init = include_str!("../../../../../opi-brc20/src/database/sql/db_init.sql");
    for table in ["brc20_events", "brc20_light_events"] {
        let index = format!("ON public.{table} USING gin (({PARTIES}));");
        assert!(init.contains(&index), "{table}");
    }
    // the wallet query reads each table with the same spelling
    let route = include_str!("../events.rs");
    assert_eq!(
        route
            .matches(&format!("where {PARTIES} && ARRAY[$1::text]"))
            .count(),
        2
    );
    assert!(
        init.contains("ON public.brc20_events USING btree (((\"event\" ->> 'tick'::text)), id);")
    );
}
