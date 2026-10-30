use {
    super::*,
    crate::{js_object, qs::parse_query},
};

const ALL: [Module; 5] = [
    Module::Brc20,
    Module::Bitmap,
    Module::Sns,
    Module::Runes,
    Module::Inscriptions,
];

fn start(query: &str, last_event_id: Option<&str>) -> Result<Start, BadParam> {
    let mut headers = HeaderMap::new();
    if let Some(id) = last_event_id {
        headers.insert("last-event-id", HeaderValue::from_str(id).unwrap());
    }
    parse(
        &parse_query(Some(query)),
        &headers,
        &ALL,
        &StreamSettings::default(),
    )
}

fn refused(query: &str, last_event_id: Option<&str>) -> String {
    match start(query, last_event_id) {
        Err(BadParam(reason)) => reason,
        Ok(_) => panic!("{query} is taken"),
    }
}

#[test]
fn modules_default_to_every_enabled_module_on_the_stream_by_name() {
    let start = start("", None).unwrap();
    assert_eq!(
        start.modules,
        [
            Provider::Bitmap,
            Provider::Brc20,
            Provider::Inscriptions,
            Provider::Sns
        ]
    );
    assert_eq!(start.confirmations, 1);
    assert_eq!(start.from_height, None);
    let start = super::tests::start("modules=sns,%20brc20,sns,", None).unwrap();
    assert_eq!(start.modules, [Provider::Brc20, Provider::Sns]);
}

#[test]
fn bad_parameters_are_refused() {
    assert_eq!(
        refused("modules=runes", None),
        "runes is not on the stream yet"
    );
    assert_eq!(
        refused("modules=ord", None),
        "modules names an unknown module: ord"
    );
    let confirmations = "confirmations must be an integer from 1 to 100";
    assert_eq!(refused("confirmations=0", None), confirmations);
    assert_eq!(refused("confirmations=101", None), confirmations);
    assert_eq!(refused("confirmations=x", None), confirmations);
    assert_eq!(
        refused("from_height=-1", None),
        "from_height must be an integer from 0 to 2147483647"
    );
    let id = "Last-Event-ID must be <module>:<height>:<hash> entries, comma-separated, of modules of the stream";
    for bad in [
        "",
        "brc20",
        "brc20:1",
        "brc20:x:h",
        "runes:1:h",
        "brc20:-1:h",
        "brc20:1:h h",
        "brc20:1:h,brc20:2:h",
        "brc20:1:h,",
    ] {
        assert_eq!(refused("", Some(bad)), id, "{bad}");
    }
    assert_eq!(refused("modules=sns", Some("brc20:1:h")), id);
}

#[test]
fn streams_read_a_pool_as_the_snapshots_do() {
    for (max_connections, readers) in [(1, 1), (2, 1), (4, 2), (10, 4), (100, 4)] {
        assert_eq!(
            super::readers(max_connections),
            readers,
            "{max_connections}"
        );
    }
}

#[test]
fn a_disabled_module_is_refused() {
    let headers = HeaderMap::new();
    let refused = parse(
        &parse_query(Some("modules=sns")),
        &headers,
        &[Module::Brc20],
        &StreamSettings::default(),
    );
    assert_eq!(
        refused.err(),
        Some(BadParam("sns is not enabled on this node".into()))
    );
    let only_runes = parse(
        &js_object! {},
        &headers,
        &[Module::Runes],
        &StreamSettings::default(),
    );
    assert_eq!(
        only_runes.err(),
        Some(BadParam("no enabled module is on the stream".into()))
    );
}

#[test]
fn a_last_event_id_names_each_module_s_position() {
    let position = |provider, height, hash: Option<&str>| Position {
        provider,
        height,
        hash: hash.map(str::to_string),
    };
    let start = start("from_height=5", Some("bitmap:100:00ab:cd")).unwrap();
    assert_eq!(
        start.resume,
        Some(vec![position(Provider::Bitmap, 100, Some("00ab:cd"))])
    );
    assert_eq!(start.from_height, Some(5));
    let id = "bitmap:7:aa,brc20:9:bb,inscriptions:8:cc,sns:6:";
    let positions = vec![
        position(Provider::Bitmap, 7, Some("aa")),
        position(Provider::Brc20, 9, Some("bb")),
        position(Provider::Inscriptions, 8, Some("cc")),
        position(Provider::Sns, 6, None),
    ];
    assert_eq!(start_resume(id), positions);
    assert_eq!(format_id(&positions), id);
}

fn start_resume(id: &str) -> Vec<Position> {
    start("", Some(id)).unwrap().resume.unwrap()
}

#[test]
fn the_block_schema_names_the_enabled_modules_blocks() {
    let schemas = components(&[Module::Sns, Module::Runes]);
    let names: Vec<&str> = schemas.iter().map(|(name, _)| name.as_str()).collect();
    for name in [
        "StreamBlock",
        "StreamRollback",
        "StreamHeartbeat",
        "StreamError",
        "StreamSnsBlock",
        "SnsName",
    ] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    assert!(!names.contains(&"StreamBrc20Block"));
    let block = &schemas
        .iter()
        .find(|(name, _)| name == "StreamBlock")
        .unwrap()
        .1;
    assert_eq!(
        serde_json::to_value(block).unwrap()["oneOf"],
        json!([{"$ref": "#/components/schemas/StreamSnsBlock"}])
    );
    let schemas = components(&[Module::Sns, Module::Inscriptions]);
    let block = &schemas
        .iter()
        .find(|(name, _)| name == "StreamBlock")
        .unwrap()
        .1;
    assert!(schemas.iter().any(|(name, _)| name == "InscriptionsEvent"));
    assert_eq!(
        serde_json::to_value(block).unwrap()["oneOf"],
        json!([
            {"$ref": "#/components/schemas/StreamInscriptionsBlock"},
            {"$ref": "#/components/schemas/StreamSnsBlock"},
        ])
    );
}
