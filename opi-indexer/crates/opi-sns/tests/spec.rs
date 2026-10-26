use std::collections::BTreeMap;

use opi_core::{BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord};
use opi_sns::json5::{NESTING_LIMIT, Value};
use opi_sns::unicode::{is_white_space, to_lowercase};
use opi_sns::{
    SnsOp, block_event_hash, candidates, content_type_accepted, cumulative_hash, is_valid_sns,
    ns_register_event_str, parse, register_event_str, sha256_hex,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
#[serde(untagged)]
enum Outcome {
    Name { name: String, domain: String },
    Namespace { namespace: String },
}

#[derive(Deserialize)]
struct Vector {
    content_hex: String,
    json5: Json,
    outcome: Option<Outcome>,
}

#[derive(Deserialize)]
struct LowerCase {
    s: String,
    lower: String,
}

#[derive(Deserialize)]
struct ChainBlock {
    block_height: u32,
    events: Vec<Vec<String>>,
    block_event_hash: String,
    cumulative_event_hash: String,
}

#[derive(Deserialize)]
struct DictOrder {
    content: String,
    items: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct SpecVectors {
    json5: String,
    unicode: String,
    nesting_limit: usize,
    white_space: Vec<u32>,
    lower: Vec<LowerCase>,
    vectors: Vec<Vector>,
    hash_chain: Vec<ChainBlock>,
    inscriptions: Vec<InscriptionCase>,
    dict_order: DictOrder,
}

#[derive(Deserialize)]
struct InscriptionCase {
    content_type_hex: String,
    number: i32,
    accepted: bool,
}

/// Rule 1 against the oracle: the module selects exactly the accepted
/// (content type, number) pairs.
#[test]
fn inscription_selection_matches_spec_vectors() {
    let cases = spec().inscriptions;
    assert!(cases.len() >= 30);
    assert!(cases.iter().any(|case| case.accepted));
    for case in cases {
        let content_type = String::from_utf8(hex::decode(&case.content_type_hex).unwrap()).unwrap();
        let view = block(vec![(
            info("ai0", case.number, "a.sats", &content_type),
            None,
        )]);
        let selected = !candidates(&view).unwrap().is_empty();
        assert_eq!(
            selected, case.accepted,
            "{content_type:?} number {}",
            case.number
        );
    }
}

fn spec() -> SpecVectors {
    serde_json::from_str(include_str!("spec_vectors.json")).expect("spec_vectors.json")
}

fn show(content: &[u8]) -> String {
    let text = String::from_utf8_lossy(content);
    let mut shown: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        shown.push('…');
    }
    format!("{shown:?}")
}

/// The oracle's canonical form of a json5 value.
fn canonical(value: &Value) -> Json {
    let text = |cps: &[u32]| -> Json {
        match cps
            .iter()
            .map(|&c| char::from_u32(c))
            .collect::<Option<String>>()
        {
            Some(s) => Json::String(s),
            None => json!({ "cps": cps }),
        }
    };
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number => json!({ "num": true }),
        Value::Str(cps) => text(cps),
        Value::List(items) => Json::Array(items.iter().map(canonical).collect()),
        Value::Object(pairs) => json!({
            "dict": pairs
                .iter()
                .map(|(k, v)| json!([text(k), canonical(v)]))
                .collect::<Vec<_>>()
        }),
    }
}

fn nesting(value: &Value) -> usize {
    match value {
        Value::List(items) => 1 + items.iter().map(nesting).max().unwrap_or(0),
        Value::Object(pairs) => 1 + pairs.iter().map(|(_, v)| nesting(v)).max().unwrap_or(0),
        _ => 0,
    }
}

#[test]
fn vectors_come_from_the_pinned_oracle() {
    let spec = spec();
    assert_eq!(spec.json5, "0.12.0");
    assert_eq!(spec.unicode, "17.0.0");
    assert_eq!(spec.nesting_limit, NESTING_LIMIT);
    assert!(spec.vectors.len() > 600);
}

#[test]
fn parse_matches_spec_vectors() {
    let mut failures = Vec::new();
    for vector in spec().vectors {
        let content = hex::decode(&vector.content_hex).unwrap();
        let expected = vector.outcome.map(|outcome| match outcome {
            Outcome::Name { name, domain } => SnsOp::Name { name, domain },
            Outcome::Namespace { namespace } => SnsOp::Namespace(namespace),
        });
        if parse(&content) != expected {
            failures.push(format!(
                "parse {}: {:?} != {expected:?}",
                show(&content),
                parse(&content)
            ));
        }
        let expected_json5 = vector.json5;
        let Ok(text) = std::str::from_utf8(&content) else {
            assert_eq!(expected_json5, "not utf-8", "{}", show(&content));
            continue;
        };
        let parsed = opi_sns::json5::parse(text);
        let agrees = match (&parsed, expected_json5.as_str()) {
            (None, Some("error")) => true,
            (Some(value), Some("deep")) => nesting(value) > 20,
            (Some(value), _) => canonical(value) == expected_json5,
            (None, _) => false,
        };
        if !agrees {
            failures.push(format!(
                "json5 {}: {:?} != {}",
                show(&content),
                parsed.as_ref().map(canonical),
                expected_json5
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn non_object_json5_registers_nothing() {
    for content in [
        "1.5",
        "\"x.sats\"",
        "'x.sats'",
        "true",
        "null",
        "[\"a.b\"]",
        "1.5e3",
        "{}",
    ] {
        assert!(opi_sns::json5::parse(content).is_some(), "{content}");
        assert_eq!(parse(content.as_bytes()), None, "{content}");
    }
    // Text that is not JSON5 is the candidate name.
    assert_eq!(parse(b"1.5.x"), None, "two dots never register");
    assert_eq!(
        parse(b"-infinity.5"),
        Some(SnsOp::Name {
            name: "-infinity.5".into(),
            domain: "5".into()
        })
    );
}

#[test]
fn namespace_with_whitespace_is_rejected() {
    for ns in [
        "my ns",
        " sats",
        "sats ",
        "sa\\tts",
        "sats\\u3000",
        "sats\\u0085",
    ] {
        let content = format!(r#"{{"p":"sns","op":"ns","ns":"{ns}"}}"#);
        assert_eq!(parse(content.as_bytes()), None, "{ns}");
    }
    assert_eq!(
        parse(br#"{"p":"sns","op":"ns","ns":"SATS"}"#),
        Some(SnsOp::Namespace("sats".into()))
    );
    // U+001C is not White_Space.
    assert_eq!(
        parse(br#"{"p":"sns","op":"ns","ns":"sa\u001cts"}"#),
        Some(SnsOp::Namespace("sa\u{1c}ts".into()))
    );
}

#[test]
fn nesting_beyond_128_is_a_parse_failure() {
    assert_eq!(NESTING_LIMIT, 128);
    let nested = |n: usize| format!("{}\"a.b\"{}", "[".repeat(n), "]".repeat(n));
    assert!(opi_sns::json5::parse(&nested(128)).is_some());
    assert!(opi_sns::json5::parse(&nested(129)).is_none());
    // A parsed list registers nothing; a failed parse makes the text the name.
    assert_eq!(parse(nested(128).as_bytes()), None);
    let deep = nested(129);
    assert_eq!(
        parse(deep.as_bytes()),
        Some(SnsOp::Name {
            name: deep.clone(),
            domain: deep.split_once('.').unwrap().1.into()
        })
    );
    let object = |n: usize| {
        format!(
            r#"{{"p":"sns","op":"reg","name":"q.r","x":{}{}}}"#,
            "[".repeat(n - 1),
            "]".repeat(n - 1)
        )
    };
    assert!(
        matches!(parse(object(128).as_bytes()), Some(SnsOp::Name { name, .. }) if name == "q.r")
    );
    assert!(
        !matches!(parse(object(129).as_bytes()), Some(SnsOp::Name { name, .. }) if name == "q.r")
    );
    // Unbounded depth fails without exhausting the stack.
    assert!(opi_sns::json5::parse(&"[".repeat(1_000_000)).is_none());
}

/// A repeated key's replaced value still counts toward the nesting limit.
#[test]
fn replaced_duplicate_key_counts_toward_nesting() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let reg = |n: usize| {
        format!(
            r#"{{"p":"sns","op":"reg","name":"a.b","x":{},"x":1}}"#,
            deep(n)
        )
    };
    let ns = |n: usize| format!(r#"{{"p":"sns","op":"ns","ns":"zz","x":{},"x":1}}"#, deep(n));
    let a_b = Some(SnsOp::Name {
        name: "a.b".into(),
        domain: "b".into(),
    });
    assert_eq!(parse(reg(127).as_bytes()), a_b);
    assert!(opi_sns::json5::parse(&reg(128)).is_none());
    assert_ne!(parse(reg(128).as_bytes()), a_b);
    assert_eq!(
        parse(ns(127).as_bytes()),
        Some(SnsOp::Namespace("zz".into()))
    );
    assert_eq!(parse(ns(128).as_bytes()), None);
}

#[test]
fn surrogate_pairs_combine() {
    let emoji = "\u{1F600}";
    let name = |body: &str| format!(r#"{{"p":"sns","op":"reg","name":"{body}"}}"#);
    assert_eq!(
        parse(name("\\ud83d\\ude00.sats").as_bytes()),
        Some(SnsOp::Name {
            name: format!("{emoji}.sats"),
            domain: "sats".into()
        })
    );
    assert_eq!(
        parse(format!(r#"{{"p":"sns","op":"ns","ns":"{}"}}"#, "\\uD83D\\uDE00").as_bytes()),
        Some(SnsOp::Namespace(emoji.into()))
    );
    // A line continuation between the halves leaves them adjacent.
    assert_eq!(
        parse(name("\\ud83d\\\n\\ude00.sats").as_bytes()),
        Some(SnsOp::Name {
            name: format!("{emoji}.sats"),
            domain: "sats".into()
        })
    );
    // Unpaired, reversed or interrupted surrogates stay lone.
    for body in [
        "\\ud83d.x",
        "\\ude00.x",
        "\\ude00\\ud83d.x",
        "\\ud83d\\u0041.x",
    ] {
        assert_eq!(parse(name(body).as_bytes()), None, "{body}");
    }
    // Keys combine too.
    let key = opi_sns::json5::parse("{\"\\ud83d\\ude00\":1}").unwrap();
    assert!(key.get(emoji).is_some());
}

#[test]
fn escaped_whitespace_in_a_json_name_is_whitespace() {
    let name = |body: &str| format!(r#"{{"p":"sns","op":"reg","name":"{body}"}}"#);
    let alice = Some(SnsOp::Name {
        name: "alice.sats".into(),
        domain: "sats".into(),
    });
    assert_eq!(parse(name("\\nalice.sats").as_bytes()), alice);
    assert_eq!(parse(name("alice.sats\\nmore").as_bytes()), alice);
    assert_eq!(parse(name("alice\\n.sats").as_bytes()), None);
    // A raw line break fails JSON5, and the raw text has no single-dot token.
    assert_eq!(parse(name("\nalice.sats").as_bytes()), None);
    assert_eq!(parse(br#"{"p":"sns","op":"ns","ns":"sa\nts"}"#), None);
}

#[test]
fn lone_surrogate_is_skipped() {
    for body in [r"\ud800.x", r"x.\udfff", "\\ude00\\ud83d.x", r"a.b\ud800c"] {
        let content = format!(r#"{{"p":"sns","op":"reg","name":"{body}"}}"#);
        assert_eq!(parse(content.as_bytes()), None, "{body}");
    }
    assert_eq!(parse(br#"{"p":"sns","op":"ns","ns":"\udc00"}"#), None);
    // Past the first whitespace a surrogate is cut away.
    assert_eq!(
        parse(br#"{"p":"sns","op":"reg","name":"a.b \ud800"}"#),
        Some(SnsOp::Name {
            name: "a.b".into(),
            domain: "b".into()
        })
    );
    let view = block(vec![(
        info(
            "ai0",
            1,
            r#"{"p":"sns","op":"reg","name":"\ud800.x"}"#,
            "text/plain",
        ),
        None,
    )]);
    assert_eq!(candidates(&view).unwrap(), Vec::new());
}

#[test]
fn name_wins_over_namespace() {
    let both = br#"{"p":"sns","op":"reg","name":"a.b","ns":"c"}"#;
    assert_eq!(
        parse(both),
        Some(SnsOp::Name {
            name: "a.b".into(),
            domain: "b".into()
        })
    );
    assert_eq!(
        parse(br#"{"p":"sns","op":"ns","ns":"zz","name":"x.y"}"#),
        Some(SnsOp::Namespace("zz".into()))
    );
}

#[test]
fn name_is_the_first_whitespace_separated_token() {
    let alice = Some(SnsOp::Name {
        name: "alice.sats".into(),
        domain: "sats".into(),
    });
    for content in [
        " alice.sats",
        "\nalice.sats",
        "\u{3000}alice.sats",
        "alice.sats \nmore",
        "\r\n\tAlice.SATS more",
    ] {
        assert_eq!(parse(content.as_bytes()), alice, "{content:?}");
    }
    for content in ["", " ", "\n", "\u{3000}", " \n\u{3000}\t"] {
        assert_eq!(parse(content.as_bytes()), None, "{content:?}");
    }
}

#[test]
fn lowercase_matches_unicode_17() {
    let spec = spec();
    assert!(spec.lower.len() > 1400);
    for case in &spec.lower {
        assert_eq!(to_lowercase(&case.s), case.lower, "{:?}", case.s);
    }
    let changed: std::collections::HashSet<&str> = spec
        .lower
        .iter()
        .map(|case| case.s.as_str())
        .filter(|s| s.chars().count() == 1)
        .collect();
    for c in (0..=0x10FFFF).filter_map(char::from_u32) {
        let s = c.to_string();
        if !changed.contains(s.as_str()) && c != 'Σ' {
            assert_eq!(to_lowercase(&s), s, "U+{:04X}", u32::from(c));
        }
    }
}

#[test]
fn white_space_is_unicode_17() {
    let spaces = spec().white_space;
    assert_eq!(spaces.len(), 25);
    for cp in 0..=0x10FFFF_u32 {
        assert_eq!(is_white_space(cp), spaces.contains(&cp), "U+{cp:04X}");
    }
}

#[test]
fn hash_chain_matches_reference_hashing() {
    let chain = spec().hash_chain;
    assert_eq!(chain.len(), 3);
    assert!(chain.iter().any(|block| block.events.is_empty()));
    let mut previous: Option<String> = None;
    for (i, block) in chain.iter().enumerate() {
        if i > 0 {
            assert_eq!(block.block_height, chain[i - 1].block_height + 1);
        }
        let events: Vec<String> = block
            .events
            .iter()
            .map(|event| match event.as_slice() {
                [kind, name, domain, id] if kind == "register" => {
                    register_event_str(name, domain, id)
                }
                [kind, namespace, id] if kind == "ns_register" => {
                    ns_register_event_str(namespace, id)
                }
                other => panic!("unknown event {other:?}"),
            })
            .collect();
        let block_hash = block_event_hash(&events);
        assert_eq!(
            block_hash, block.block_event_hash,
            "block {}",
            block.block_height
        );
        let cumulative = cumulative_hash(previous.as_deref(), &block_hash);
        assert_eq!(
            cumulative, block.cumulative_event_hash,
            "block {}",
            block.block_height
        );
        previous = Some(cumulative);
    }
}

fn text_pairs(value: &Value) -> Vec<(String, String)> {
    let Value::Object(pairs) = value else {
        panic!("{value:?} is not an object");
    };
    let text = |cps: &[u32]| cps.iter().map(|&c| char::from_u32(c).unwrap()).collect();
    pairs
        .iter()
        .map(|(k, v)| {
            let Value::Str(v) = v else { panic!("{v:?}") };
            (text(k), text(v))
        })
        .collect()
}

/// A repeated key keeps its first position and its last value.
#[test]
fn duplicate_keys_keep_first_position_and_last_value() {
    let case = spec().dict_order;
    let value = opi_sns::json5::parse(&case.content).expect("object");
    assert_eq!(text_pairs(&value), case.items);
}

/// Distinct keys build in linear time. Debug builds are too slow to bound
/// this; run with `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn many_distinct_keys_parse_quickly() {
    let body: Vec<String> = (0..200_000).map(|i| format!("k{i}:'v'")).collect();
    let content = format!("{{{}}}", body.join(","));
    let started = std::time::Instant::now();
    let value = opi_sns::json5::parse(&content).expect("object");
    let elapsed = started.elapsed();
    assert_eq!(text_pairs(&value).len(), 200_000);
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "{elapsed:?}"
    );
}

fn info(id: &str, number: i32, content: &str, content_type: &str) -> InscriptionInfo {
    InscriptionInfo {
        inscription_id: id.into(),
        inscription_number: number,
        cursed_for_brc20: false,
        parent_id: None,
        is_json: false,
        content: content.as_bytes().to_vec(),
        content_type: content_type.as_bytes().to_vec(),
        metaprotocol: Vec::new(),
    }
}

fn record(idx: u32, id: &str, old: Option<&str>) -> InscriptionRecord {
    InscriptionRecord {
        height: 0,
        idx,
        inscription_id: id.into(),
        old_satpoint: old.map(String::from),
        new_satpoint: "tx:0:0".into(),
        sent_as_fee: false,
        new_output_value: 546,
        txid: "tx".into(),
        new_pkscript: Vec::new(),
    }
}

fn block(entries: Vec<(InscriptionInfo, Option<&str>)>) -> BlockView {
    let mut records = Vec::new();
    let mut infos = BTreeMap::new();
    for (idx, (info, old)) in entries.into_iter().enumerate() {
        records.push(record(idx as u32, &info.inscription_id, old));
        infos.insert(info.inscription_id.clone(), info);
    }
    BlockView {
        height: 900,
        hash: "00".repeat(32),
        time: 0,
        inscriptions: InscriptionFeed { records, infos },
    }
}

#[test]
fn content_types_are_text_plain_or_application_json() {
    let plain = |ct: &str| info("a", 1, "x", ct);
    assert!(is_valid_sns(&plain("text/plain")));
    assert!(is_valid_sns(&plain("application/json;charset=utf-8")));
    // The prefilter matches a prefix; content_type_accepted is exact.
    assert!(is_valid_sns(&plain("text/plainx")));
    assert!(!is_valid_sns(&plain("text/html")));
    assert!(!is_valid_sns(&info("a", -1, "x", "text/plain")));
    assert!(content_type_accepted(b"text/plain"));
    assert!(content_type_accepted(b"text/plain;charset=utf-8"));
    assert!(content_type_accepted(b"application/json"));
    assert!(content_type_accepted(b"application/json;"));
    assert!(!content_type_accepted(b"text/plainx"));
    assert!(!content_type_accepted(b"text/plain ;"));
    assert!(!content_type_accepted(b"Text/plain"));
}

#[test]
fn candidates_are_blessed_creations_in_ascending_number_order() {
    let view = block(vec![
        (info("ci0", 30, "c.sats", "text/plain"), None),
        (info("ai0", 10, "a.sats", "text/plain;charset=utf-8"), None),
        (info("bi0", 20, "b.sats", "text/plain"), Some("tx:1:0")),
        (info("di0", -4, "d.sats", "text/plain"), None),
        (info("ei0", 5, "e.sats", "text/plainx"), None),
        (
            info(
                "fi0",
                7,
                r#"{"p":"sns","op":"ns","ns":"F"}"#,
                "application/json",
            ),
            None,
        ),
        (info("gi0", 8, "nothing", "text/plain"), None),
    ]);
    let found: Vec<_> = candidates(&view)
        .unwrap()
        .into_iter()
        .map(|c| (c.inscription_id, c.op))
        .collect();
    let name = |n: &str| SnsOp::Name {
        name: n.into(),
        domain: "sats".into(),
    };
    assert_eq!(
        found,
        vec![
            ("fi0".to_string(), SnsOp::Namespace("f".into())),
            ("ai0".to_string(), name("a.sats")),
            ("ci0".to_string(), name("c.sats")),
        ]
    );
}

#[test]
fn creation_without_info_is_an_error_naming_it() {
    let mut view = block(vec![(info("ai0", 1, "a.sats", "text/plain"), None)]);
    view.inscriptions.infos.clear();
    let error = candidates(&view).unwrap_err().to_string();
    assert!(error.contains("ai0"), "{error}");
}

#[test]
fn event_strings_and_hashes() {
    assert_eq!(register_event_str("a.b", "b", "xi0"), "register;xi0;a.b;b");
    assert_eq!(ns_register_event_str("b", "xi0"), "ns_register;xi0;b");
    assert_eq!(
        block_event_hash(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let events = vec!["register;xi0;a.b;b".to_string(), "ns_register;yi0;c".into()];
    assert_eq!(
        block_event_hash(&events),
        sha256_hex("register;xi0;a.b;b|ns_register;yi0;c")
    );
}
