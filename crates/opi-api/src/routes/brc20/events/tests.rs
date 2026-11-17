//! The event feeds' parameter rules.

use {
    super::*,
    crate::{
        qs::parse_query,
        routes::paging::{CATEGORY_PATTERN, WALLET_CATEGORY_PATTERN},
    },
};

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
            categories: None,
        })
    );
    assert_eq!(
        FeedRequest::from_query(&query(
            "limit=500&before_id=42&type=mint-inscribe&category=wrap"
        )),
        Ok(FeedRequest {
            limit: 500,
            before_id: Some(42),
            event_type: Some("mint-inscribe".into()),
            categories: Some(vec!["wrap".into()]),
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
        (
            "type=a%00",
            bad("type must be at most 256 bytes without NUL"),
        ),
    ] {
        assert_eq!(FeedRequest::from_query(&query(text)), expected, "{text}");
    }
}

#[test]
fn the_wallet_feed_takes_a_block_cursor_and_transfers() {
    let parse = |text: &str| WalletFeedRequest::from_query(&query(text));
    assert_eq!(
        parse(""),
        Ok(WalletFeedRequest {
            limit: DEFAULT_LIMIT,
            before_id: None,
            before: None,
            event_type: None,
            categories: None,
            transfers: false,
        })
    );
    assert_eq!(
        parse("include_prog_transfers=true&before=840000:3&limit=200"),
        Ok(WalletFeedRequest {
            limit: 200,
            before_id: None,
            before: Some(BlockCursor {
                block_height: 840_000,
                rows: 3,
            }),
            event_type: None,
            categories: None,
            transfers: true,
        })
    );
    let listed = |text: &str| {
        let request = parse(text).unwrap();
        (request.categories, request.transfers)
    };
    // prog_transfer lists the transfers; the rest are event categories
    assert_eq!(listed("category=prog_transfer"), (Some(vec![]), true));
    assert_eq!(
        listed("category=prog_transfer,mint"),
        (Some(vec!["mint".into()]), true)
    );
    assert_eq!(
        listed("category=mint&include_prog_transfers=true"),
        (Some(vec!["mint".into()]), true)
    );
    assert_eq!(
        listed("category=mint&include_prog_transfers=false"),
        (Some(vec!["mint".into()]), false)
    );
    assert_eq!(listed("include_prog_transfers="), (None, false));
    assert_eq!(
        parse(&format!("before=2147483647:{MAX_BEFORE_ID}"))
            .unwrap()
            .before,
        Some(BlockCursor {
            block_height: i32::MAX,
            rows: MAX_BEFORE_ID,
        })
    );
    let cursor = "before must be <block_height>:<rows>, the next_before of the previous page";
    for (text, expected) in [
        ("before=", bad(cursor)),
        ("before=1", bad(cursor)),
        ("before=1:", bad(cursor)),
        ("before=:1", bad(cursor)),
        ("before=+1:1", bad(cursor)),
        ("before=1:1:1", bad(cursor)),
        ("before=2147483648:0", bad(cursor)),
        ("before=1:1000000000000000000", bad(cursor)),
        (
            "before=1:2&before=3:4",
            bad("before must be a single value"),
        ),
        (
            "before=1:2&before_id=3",
            bad("give before or before_id, not both"),
        ),
        (
            "before_id=3&category=prog_transfer",
            bad("before_id does not page BRC2.0 transfers; use before"),
        ),
        (
            "include_prog_transfers=1",
            bad("include_prog_transfers must be true or false"),
        ),
        (
            "category=nope",
            bad(
                "category must be one or more of deploy, mint, transfer, wrap, unwrap, brc20prog, prog_transfer, separated by commas",
            ),
        ),
    ] {
        assert_eq!(parse(text), expected, "{text}");
    }
    // the documented pattern takes what the parser takes
    assert_eq!(
        WALLET_CATEGORY_PATTERN,
        format!(
            "^(({names})(,({names}))*)?$",
            names = WALLET_CATEGORIES.join("|")
        )
    );
    assert_eq!(&WALLET_CATEGORIES[..CATEGORIES.len()], &CATEGORIES[..]);
}

#[test]
fn a_page_ends_at_its_last_rows_block_cursor() {
    // the rows' blocks, newest first; the count includes the rows of the
    // cursor's block that earlier pages gave
    assert_eq!(
        block_cursor_after(&[301, 301, 250], None, 0),
        BlockCursor {
            block_height: 250,
            rows: 1
        }
    );
    assert_eq!(
        block_cursor_after(&[301, 301], Some(301), 4),
        BlockCursor {
            block_height: 301,
            rows: 6
        }
    );
    assert_eq!(
        block_cursor_after(&[250, 200, 200], Some(301), 7),
        BlockCursor {
            block_height: 200,
            rows: 2
        }
    );
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
            bad("address must be at most 128 bytes without NUL"),
        ),
        (
            "address=a%00",
            bad("address must be at most 128 bytes without NUL"),
        ),
        (long_pkscript.as_str(), bad(pkscript)),
    ] {
        assert_eq!(Wallet::from_query(&query(text)), expected, "{text}");
    }
}

#[test]
fn categories_are_a_comma_separated_list() {
    let categories = |text: &str| category_list(&query(text), &CATEGORIES);
    assert_eq!(categories(""), Ok(None));
    assert_eq!(categories("category="), Ok(None));
    // kept in the order CATEGORIES lists them, without repeats
    assert_eq!(
        categories("category=unwrap,deploy,wrap,deploy"),
        Ok(Some(vec!["deploy".into(), "wrap".into(), "unwrap".into()]))
    );
    assert_eq!(
        categories("category=deploy,mint,transfer,wrap,unwrap,brc20prog"),
        Ok(Some(CATEGORIES.map(String::from).to_vec()))
    );
    let names = "category must be one or more of deploy, mint, transfer, wrap, unwrap, brc20prog, separated by commas";
    for text in [
        "category=Deploy",
        "category=deploy,",
        "category=,deploy",
        "category=deploy wrap",
        "category=nope",
    ] {
        assert_eq!(categories(text), bad(names), "{text}");
    }
    assert_eq!(
        categories("category=wrap&category=unwrap"),
        bad("category must be a single value")
    );
    assert_eq!(
        categories(&format!("category={}", "wrap,".repeat(13))),
        bad("category must be at most 64 bytes without NUL")
    );
    // the documented pattern takes what the parser takes
    assert_eq!(
        CATEGORY_PATTERN,
        format!("^(({names})(,({names}))*)?$", names = CATEGORIES.join("|"))
    );
}

#[test]
fn the_category_function_names_the_categories_and_the_module() {
    let init = include_str!("../../../../../opi-brc20/src/database/sql/db_init.sql");
    let function = &init[init.find("FUNCTION public.brc20_event_category").unwrap()..];
    let function = &function[..function.find("$$;").unwrap()];
    for category in CATEGORIES {
        assert!(function.contains(&format!("'{category}'")), "{category}");
    }
    assert!(function.contains(&format!(
        "(e ->> 'spent_pkScript') = '{}'",
        super::super::tickers::BRC20_PROG_MODULE_PKSCRIPT
    )));
    // the rare categories' partial indexes, which the feeds' filter matches
    for table in ["brc20_events", "brc20_light_events"] {
        for category in ["deploy", "wrap", "unwrap"] {
            assert!(init.contains(&format!(
                "{table}_{category}_ticker_id_idx ON public.{table} USING btree (((\"event\" ->> 'tick'::text)), id) WHERE (public.brc20_event_category(event_type, \"event\") = '{category}'::text);"
            )));
        }
        for category in ["deploy", "wrap", "unwrap", "brc20prog"] {
            assert!(init.contains(&format!(
                "{table}_{category}_id_idx ON public.{table} USING btree (id) WHERE (public.brc20_event_category(event_type, \"event\") = '{category}'::text);"
            )));
        }
    }
    let route = include_str!("../events.rs");
    assert_eq!(
        route
            .matches("brc20_event_category(event_type, event) = any(")
            .count(),
        4
    );
}

#[test]
fn an_address_is_read_as_its_pkscript() {
    let key = address_key("tb1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdplwdcv3").unwrap();
    assert_eq!(key.pkscript, format!("0014{}", "a1".repeat(20)));
    assert_eq!(
        key.wallet.as_deref(),
        Some("tb1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdplwdcv3")
    );
    // the mainnet form of the same program encodes the same pkScript; the
    // wallet match tells the two apart
    assert_eq!(
        address_key("bc1q5xs6rgdp5xs6rgdp5xs6rgdp5xs6rgdp4gkthz")
            .unwrap()
            .pkscript,
        key.pkscript
    );
    assert!(address_key("tb1qalice").is_none());
    assert!(address_key("").is_none());
}
