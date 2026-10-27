//! The ticker routes' parameter rules.

use {super::*, crate::qs::parse_query};

fn query(text: &str) -> JsObject {
    parse_query(Some(text))
}

fn bad(text: &str) -> Result<Page, BadParam> {
    Err(BadParam(text.into()))
}

#[test]
fn pages_default_and_cap() {
    assert_eq!(
        Page::from_query(&query("")),
        Ok(Page {
            offset: 0,
            limit: DEFAULT_LIMIT
        })
    );
    assert_eq!(
        Page::from_query(&query("offset=1000&limit=500")),
        Ok(Page {
            offset: 1000,
            limit: 500
        })
    );
    let limit = "limit must be an integer from 1 to 500";
    let offset = "offset must be an integer from 0 to 1000000000";
    for (text, expected) in [
        ("limit=0", bad(limit)),
        ("limit=501", bad(limit)),
        ("limit=-1", bad(limit)),
        ("limit=1e2", bad(limit)),
        ("limit=%2010", bad(limit)),
        ("limit=", bad(limit)),
        ("limit=0x10", bad(limit)),
        ("offset=-1", bad(offset)),
        ("offset=1000000001", bad(offset)),
        ("offset=99999999999999999999", bad(offset)),
        ("limit=1&limit=2", bad("limit must be a single value")),
        ("offset[a]=1", bad("offset must be a single value")),
    ] {
        assert_eq!(Page::from_query(&query(text)), expected, "{text}");
    }
}

#[test]
fn a_page_fetches_one_more_row_to_tell_whether_more_follow() {
    let page = Page {
        offset: 0,
        limit: 2,
    };
    assert_eq!(page.fetch(), 3);
    assert_eq!(page.split(vec![1, 2, 3]), (vec![1, 2], true));
    assert_eq!(page.split(vec![1, 2]), (vec![1, 2], false));
    assert_eq!(page.split(Vec::<i32>::new()), (vec![], false));
}

#[test]
fn tickers_are_required_and_lowercased() {
    assert_eq!(ticker(&query("ticker=OrDi"), "ticker"), Ok("ordi".into()));
    assert_eq!(
        ticker(&query("ticker=%C3%84bc"), "ticker"),
        Ok("äbc".into())
    );
    for text in ["", "ticker=", "x=ordi"] {
        assert_eq!(
            ticker(&query(text), "ticker"),
            Err(BadParam("ticker is required".into())),
            "{text}"
        );
    }
    assert_eq!(
        ticker(&query("ticker=a&ticker=b"), "ticker"),
        Err(BadParam("ticker must be a single value".into()))
    );
}

#[test]
fn ticker_lists_take_whitelisted_orders() {
    assert_eq!(
        TickersRequest::from_query(&query("")),
        Ok(TickersRequest {
            sort: TickerSort::Holders,
            ascending: false,
            pattern: None,
            page: Page {
                offset: 0,
                limit: DEFAULT_LIMIT
            },
        })
    );
    for sort in TickerSort::ALL {
        let request =
            TickersRequest::from_query(&query(&format!("sort={}&order=asc", sort.name()))).unwrap();
        assert_eq!((request.sort, request.ascending), (sort, true));
    }
    assert_eq!(
        TickersRequest::from_query(&query("sort=price")),
        Err(BadParam(
            "sort must be one of holders, prog_holders, max_supply, mint_progress, deploy_height"
                .into()
        ))
    );
    assert_eq!(
        TickersRequest::from_query(&query("order=ASC")),
        Err(BadParam("order must be asc or desc".into()))
    );
    assert_eq!(
        TickersRequest::from_query(&query("limit=600")),
        Err(BadParam("limit must be an integer from 1 to 500".into()))
    );
}

#[test]
fn search_is_a_lowercase_prefix_with_like_characters_escaped() {
    let pattern = |text: &str| TickersRequest::from_query(&query(text)).unwrap().pattern;
    assert_eq!(pattern("search=OR"), Some("or%".into()));
    assert_eq!(pattern("search="), None);
    assert_eq!(pattern("search=a%25_b%5C"), Some("a\\%\\_b\\\\%".into()));
}

#[test]
fn evm_addresses_are_0x_and_40_hex_digits() {
    assert!(is_evm_address("0xAbCdEf0123456789aBcDeF0123456789AbCdEf01"));
    assert!(!is_evm_address("0xAbCdEf0123456789aBcDeF0123456789AbCdEf0"));
    assert!(!is_evm_address(
        "0xAbCdEf0123456789aBcDeF0123456789AbCdEfg1"
    ));
    assert!(!is_evm_address(
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
    ));
}
