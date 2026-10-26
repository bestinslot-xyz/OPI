use std::collections::BTreeMap;

use opi_core::{BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord};
use opi_sns::json5::{FRAME_LIMIT, frame_stats};
use opi_sns::pystr::{is_py_space, py_lower, py_split_first, py_strip};
use opi_sns::{
    SnsOp, block_event_hash, candidates, content_type_accepted, cumulative_hash, get_ns_register,
    get_sns_name, is_valid_sns, ns_register_event_str, parse, register_event_str, sha256_hex,
};
use serde::Deserialize;

/// A Python string: text, or code points when it holds lone surrogates.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum PyStr {
    Text(String),
    CodePoints(Vec<u32>),
}

impl PyStr {
    fn cps(&self) -> Vec<u32> {
        match self {
            PyStr::Text(s) => s.chars().map(u32::from).collect(),
            PyStr::CodePoints(cps) => cps.clone(),
        }
    }

    fn text(&self) -> &str {
        match self {
            PyStr::Text(s) => s,
            PyStr::CodePoints(cps) => panic!("{cps:?} is not encodable"),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Outcome {
    Stall(String),
    Name { name: PyStr, domain: PyStr },
    Namespace { namespace: PyStr },
}

#[derive(Deserialize)]
struct Vector {
    content_hex: String,
    name: Option<PyStr>,
    domain: Option<PyStr>,
    namespace: Option<PyStr>,
    outcome: Option<Outcome>,
    json5_depth: Option<u32>,
    json5_calls: Option<u64>,
}

#[derive(Deserialize)]
struct LowerCase {
    s: PyStr,
    lower: PyStr,
}

#[derive(Deserialize)]
struct Vectors {
    json5: String,
    recursion_limit: u32,
    isspace: Vec<u32>,
    lower: Vec<LowerCase>,
    sns: Vec<Vector>,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("vectors.json")).expect("vectors.json")
}

fn show(content: &[u8]) -> String {
    let text = String::from_utf8_lossy(content);
    let mut shown: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        shown.push('…');
    }
    format!("{shown:?}")
}

#[test]
fn vectors_come_from_the_pinned_python() {
    let vectors = vectors();
    assert_eq!(vectors.json5, "0.12.0");
    assert_eq!(vectors.recursion_limit, 1000);
    assert!(vectors.sns.len() > 500);
}

#[test]
fn parse_matches_python_vectors() {
    let mut failures = Vec::new();
    for vector in vectors().sns {
        let content = hex::decode(&vector.content_hex).unwrap();
        let expected_name = vector
            .name
            .as_ref()
            .map(|name| (name.cps(), vector.domain.as_ref().unwrap().cps()));
        if get_sns_name(&content) != expected_name {
            failures.push(format!(
                "get_sns_name {}: {:?} != {:?}",
                show(&content),
                get_sns_name(&content),
                expected_name
            ));
        }
        let expected_namespace = vector.namespace.as_ref().map(PyStr::cps);
        if get_ns_register(&content) != expected_namespace {
            failures.push(format!(
                "get_ns_register {}: {:?} != {:?}",
                show(&content),
                get_ns_register(&content),
                expected_namespace
            ));
        }
        let expected = match &vector.outcome {
            None => Ok(None),
            Some(Outcome::Stall(stall)) => {
                assert_eq!(stall, "stall");
                Err(opi_sns::Unencodable)
            }
            Some(Outcome::Name { name, domain }) => Ok(Some(SnsOp::Name {
                name: name.text().into(),
                domain: domain.text().into(),
            })),
            Some(Outcome::Namespace { namespace }) => {
                Ok(Some(SnsOp::Namespace(namespace.text().into())))
            }
        };
        if parse(&content) != expected {
            failures.push(format!(
                "parse {}: {:?} != {:?}",
                show(&content),
                parse(&content),
                expected
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The parser makes the same calls as json5's and stacks them as deep, so it
/// meets Python's recursion limit on the same inputs.
#[test]
fn json5_frames_match_python() {
    let mut failures = Vec::new();
    let mut at_limit = 0;
    for vector in vectors().sns {
        let content = hex::decode(&vector.content_hex).unwrap();
        let (Some(expected), Some(expected_calls)) = (vector.json5_depth, vector.json5_calls)
        else {
            continue;
        };
        let content = String::from_utf8(content).unwrap();
        let (depth, calls) = frame_stats(&content);
        if (depth, calls) != (expected, expected_calls) {
            failures.push(format!(
                "{}: depth {depth}, calls {calls} != depth {expected}, calls {expected_calls}",
                show(content.as_bytes())
            ));
        }
        if expected == FRAME_LIMIT || expected == FRAME_LIMIT + 1 {
            at_limit += 1;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        at_limit > 0,
        "the vectors pin both sides of the frame limit"
    );
}

#[test]
fn py_lower_matches_python_for_special_cases() {
    let vectors = vectors();
    let mut checked = 0;
    for case in &vectors.lower {
        let (PyStr::Text(s), PyStr::Text(lower)) = (&case.s, &case.lower) else {
            continue;
        };
        assert_eq!(&py_lower(s), lower, "{s:?}");
        checked += 1;
    }
    assert!(checked > 1400);
    assert_eq!(py_lower("ΣΑΣ.sats"), "σασ.sats");
    assert_eq!(py_lower("ΑΣ"), "ας");
    assert_eq!(py_lower("İ.sats"), "i\u{307}.sats");
    let changed: std::collections::HashSet<String> = vectors
        .lower
        .iter()
        .filter_map(|case| match &case.s {
            PyStr::Text(s) if s.chars().count() == 1 => Some(s.clone()),
            _ => None,
        })
        .collect();
    for c in (0..=0x10FFFF).filter_map(char::from_u32) {
        let s = c.to_string();
        if !changed.contains(&s) && c != 'Σ' {
            assert_eq!(py_lower(&s), s, "U+{:04X}", u32::from(c));
        }
    }
}

#[test]
fn py_split_uses_python_whitespace() {
    let spaces = vectors().isspace;
    for cp in 0..=0x10FFFF_u32 {
        assert_eq!(is_py_space(cp), spaces.contains(&cp), "U+{cp:04X}");
    }
    assert!(spaces.contains(&0x1C) && !char::from_u32(0x1C).unwrap().is_whitespace());
    for &cp in &spaces {
        let space = char::from_u32(cp).unwrap();
        let s = format!("{space}{space}alice.sats{space}x{space}");
        assert_eq!(py_split_first(&s), Some("alice.sats"), "U+{cp:04X}");
        assert_eq!(py_strip(&s), format!("alice.sats{space}x"), "U+{cp:04X}");
    }
    assert_eq!(py_split_first(""), None);
    assert_eq!(py_split_first("\u{1c}\u{85} "), None);
    assert_eq!(py_split_first("a\u{200b}b c"), Some("a\u{200b}b"));
}

#[test]
fn name_wins_over_namespace() {
    let both = br#"{"p":"sns","op":"reg","name":"a.b","ns":"c"}"#;
    assert_eq!(get_ns_register(both), None);
    assert_eq!(
        parse(both),
        Ok(Some(SnsOp::Name {
            name: "a.b".into(),
            domain: "b".into()
        }))
    );
    // A name rejected by index_block does not fall back to the namespace.
    let ns = br#"{"p":"sns","op":"ns","ns":"zz"}"#;
    assert_eq!(parse(ns), Ok(Some(SnsOp::Namespace("zz".into()))));
    let nul_name = b"a\x00.b";
    assert!(get_sns_name(nul_name).is_some());
    assert_eq!(parse(nul_name), Ok(None));
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
fn content_types_follow_db_reader_and_index_block() {
    let plain = |ct: &str| info("a", 1, "x", ct);
    assert!(is_valid_sns(&plain("text/plain")));
    assert!(is_valid_sns(&plain("application/json;charset=utf-8")));
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
fn candidates_are_valid_creations_in_ascending_number_order() {
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
    assert_eq!(
        found,
        vec![
            ("fi0".to_string(), SnsOp::Namespace("f".into())),
            (
                "ai0".to_string(),
                SnsOp::Name {
                    name: "a.sats".into(),
                    domain: "sats".into()
                }
            ),
            (
                "ci0".to_string(),
                SnsOp::Name {
                    name: "c.sats".into(),
                    domain: "sats".into()
                }
            ),
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
fn unencodable_name_is_an_error_naming_the_inscription() {
    let view = block(vec![(
        info(
            "ai0",
            1,
            r#"{"p":"sns","op":"reg","name":"\ud800.x"}"#,
            "text/plain",
        ),
        None,
    )]);
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
    let first = sha256_hex("x");
    assert_eq!(cumulative_hash(None, &first), first);
    assert_eq!(
        cumulative_hash(Some(&first), "y"),
        sha256_hex(&format!("{first}y"))
    );
}
