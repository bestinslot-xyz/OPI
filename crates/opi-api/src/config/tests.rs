//! The Node API's config cases, plus the parseInt and `||` rules it applies
//! to each variable.

use {super::*, std::collections::HashMap};

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |key| map.get(key).cloned()
}

fn err(result: anyhow::Result<impl std::fmt::Debug>) -> String {
    format!("{:#}", result.unwrap_err())
}

#[test]
fn the_stream_settings_default_and_are_checked() {
    let base = [("DB_DATABASE", "d")];
    let config = load_config(env(&base)).unwrap();
    assert_eq!(config.stream.settings().unwrap(), StreamSettings::default());
    let config = load_config(env(&[
        ("DB_DATABASE", "d"),
        ("STREAM_POLL_MS", "250"),
        ("STREAM_DEFAULT_CONFIRMATIONS", "3"),
        ("STREAM_MAX_CONFIRMATIONS", "1000"),
        ("STREAM_MAX_SUBSCRIBERS", "0"),
        ("STREAM_MAX_PER_CLIENT", "2"),
        ("STREAM_HEARTBEAT_SECS", "5"),
        ("STREAM_BUFFER", "2"),
        ("STREAM_RESUME_WAIT_SECS", "30"),
    ]))
    .unwrap();
    assert_eq!(
        config.stream.settings().unwrap(),
        StreamSettings {
            poll: Duration::from_millis(250),
            default_confirmations: 3,
            max_confirmations: 1000,
            max_subscribers: 0,
            max_per_client: 2,
            heartbeat: Duration::from_secs(5),
            buffer: 2,
            resume_wait: Duration::from_secs(30),
        }
    );
    for (var, value, message) in [
        (
            "STREAM_MAX_CONFIRMATIONS",
            "1001",
            "STREAM_MAX_CONFIRMATIONS must be an integer from 1 to 1000, got 1001",
        ),
        (
            "STREAM_DEFAULT_CONFIRMATIONS",
            "101",
            "STREAM_DEFAULT_CONFIRMATIONS must be an integer from 1 to 100, got 101",
        ),
        (
            "STREAM_POLL_MS",
            "x",
            "STREAM_POLL_MS must be an integer from 10 to 3600000, got NaN",
        ),
        (
            "STREAM_HEARTBEAT_SECS",
            "0",
            "STREAM_HEARTBEAT_SECS must be an integer from 1 to 3600, got 0",
        ),
        (
            "STREAM_BUFFER",
            "0",
            "STREAM_BUFFER must be an integer from 1 to 65536, got 0",
        ),
        (
            "STREAM_MAX_SUBSCRIBERS",
            "-1",
            "STREAM_MAX_SUBSCRIBERS must be an integer from 0 to 1000000, got -1",
        ),
        (
            "STREAM_MAX_PER_CLIENT",
            "0",
            "STREAM_MAX_PER_CLIENT must be an integer from 1 to 1000000, got 0",
        ),
        (
            "STREAM_RESUME_WAIT_SECS",
            "0",
            "STREAM_RESUME_WAIT_SECS must be an integer from 1 to 86400, got 0",
        ),
    ] {
        let config = load_config(env(&[("DB_DATABASE", "d"), (var, value)])).unwrap();
        assert_eq!(err(config.validate()), message);
    }
}

#[test]
fn parse_modules_defaults_to_brc20_when_unset() {
    assert_eq!(parse_modules(None).unwrap(), [Module::Brc20]);
}

#[test]
fn parse_modules_trims_ignores_empty_entries_and_dedups() {
    assert_eq!(
        parse_modules(Some(" sns , ,runes,sns,brc20 ")).unwrap(),
        [Module::Sns, Module::Runes, Module::Brc20]
    );
}

#[test]
fn parse_modules_trims_js_whitespace() {
    // String.prototype.trim strips U+FEFF and keeps U+0085.
    assert_eq!(
        parse_modules(Some("\u{feff}sns\u{a0}")).unwrap(),
        [Module::Sns]
    );
    assert!(err(parse_modules(Some("\u{85}sns"))).contains("unknown module `\u{85}sns`"));
}

#[test]
fn parse_modules_rejects_an_unknown_module() {
    assert_eq!(
        err(parse_modules(Some("brc20,ordinals"))),
        "unknown module `ordinals` in OPI_MODULES (supported: brc20, bitmap, sns, runes, inscriptions)"
    );
}

#[test]
fn parse_modules_rejects_an_empty_list() {
    for value in [" , ", ""] {
        assert_eq!(
            err(parse_modules(Some(value))),
            "OPI_MODULES names no module (supported: brc20, bitmap, sns, runes, inscriptions)"
        );
    }
}

#[test]
fn load_config_requires_db_database_for_brc20() {
    assert_eq!(
        err(load_config(env(&[]))),
        "DB_DATABASE is not set (required because brc20 is enabled)"
    );
    // an empty value counts as unset
    assert!(err(load_config(env(&[("DB_DATABASE", "")]))).contains("DB_DATABASE is not set"));
}

#[test]
fn load_config_requires_a_database_name_per_enabled_module() {
    let base = [
        ("OPI_MODULES", "brc20,bitmap,sns,runes,inscriptions"),
        ("DB_DATABASE", "b"),
    ];
    assert!(err(load_config(env(&base))).contains("BITMAP_DB_DATABASE is not set"));
    let with_bitmap = [base[0], base[1], ("BITMAP_DB_DATABASE", "m")];
    assert!(err(load_config(env(&with_bitmap))).contains("SNS_DB_DATABASE is not set"));
    let with_sns = [
        base[0],
        base[1],
        ("BITMAP_DB_DATABASE", "m"),
        ("SNS_DB_DATABASE", "s"),
    ];
    assert_eq!(
        err(load_config(env(&with_sns))),
        "RUNES_DB_DATABASE is not set (required because runes is enabled)"
    );
    let with_runes = [
        base[0],
        base[1],
        ("BITMAP_DB_DATABASE", "m"),
        ("SNS_DB_DATABASE", "s"),
        ("RUNES_DB_DATABASE", "r"),
    ];
    assert_eq!(
        err(load_config(env(&with_runes))),
        "INSCRIPTIONS_DB_DATABASE is not set (required because inscriptions is enabled)"
    );
}

#[test]
fn load_config_ignores_the_settings_of_disabled_modules() {
    let config = load_config(env(&[("OPI_MODULES", "sns"), ("SNS_DB_DATABASE", "s")])).unwrap();
    assert_eq!(config.modules, [Module::Sns]);
    assert_eq!(
        config.dbs.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
        [Module::Sns]
    );
}

#[test]
fn load_config_reads_prefixed_and_unprefixed_db_variables() {
    let config = load_config(env(&[
        ("OPI_MODULES", "brc20,runes"),
        ("DB_DATABASE", "b"),
        ("DB_USER", "bu"),
        ("DB_HOST", "bh"),
        ("DB_PORT", "5433"),
        ("DB_PASSWD", "bp"),
        ("DB_SSL", "true"),
        ("RUNES_DB_DATABASE", "r"),
        ("RUNES_DB_USER", "ru"),
        ("DB_MAX_CONNECTIONS", "3"),
    ]))
    .unwrap();
    assert_eq!(
        config.db(Module::Brc20).unwrap(),
        &DbConfig {
            prefix: "DB",
            user: "bu".into(),
            host: "bh".into(),
            port: 5433.0,
            database: "b".into(),
            password: Some("bp".into()),
            ssl: true,
            max: 3.0,
            connection_timeout_ms: 5000,
        }
    );
    assert_eq!(
        config.db(Module::Runes).unwrap(),
        &DbConfig {
            prefix: "RUNES_DB",
            user: "ru".into(),
            host: "localhost".into(),
            port: 5432.0,
            database: "r".into(),
            password: None,
            ssl: false,
            max: 3.0,
            connection_timeout_ms: 5000,
        }
    );
}

#[test]
fn load_config_applies_api_and_rate_limit_defaults() {
    let config = load_config(env(&[("DB_DATABASE", "b")])).unwrap();
    assert_eq!(config.api.host, "127.0.0.1");
    assert_eq!(config.api.port, 8000.0);
    assert_eq!(config.api.trusted_proxies, 0.0);
    assert_eq!(
        config.rate_limit,
        RateLimitSettings {
            enabled: false,
            window_ms: 900000.0,
            max: 100.0
        }
    );
    assert_eq!(config.metrics_addr, None);
}

#[test]
fn the_metrics_listener_runs_only_with_an_address() {
    let metrics_addr = |value: &str| {
        load_config(env(&[("DB_DATABASE", "b"), ("API_METRICS_ADDR", value)]))
            .unwrap()
            .metrics_addr
    };
    assert_eq!(metrics_addr(""), None);
    assert_eq!(
        metrics_addr("127.0.0.1:9101").as_deref(),
        Some("127.0.0.1:9101")
    );
}

#[test]
fn load_config_reads_api_and_rate_limit_settings() {
    let config = load_config(env(&[
        ("DB_DATABASE", "b"),
        ("API_HOST", "0.0.0.0"),
        ("API_PORT", "9000"),
        ("API_TRUSTED_PROXY_CNT", "2"),
        ("RATE_LIMIT_ENABLE", "true"),
        ("RATE_LIMIT_WINDOW_MS", "1000"),
        ("RATE_LIMIT_MAX", "5"),
    ]))
    .unwrap();
    assert_eq!(config.api.host, "0.0.0.0");
    assert_eq!(config.api.port, 9000.0);
    assert_eq!(config.api.trusted_proxies, 2.0);
    assert_eq!(
        config.rate_limit,
        RateLimitSettings {
            enabled: true,
            window_ms: 1000.0,
            max: 5.0
        }
    );
}

#[test]
fn numbers_go_through_parse_int_and_empty_values_take_the_default() {
    let config = load_config(env(&[
        ("DB_DATABASE", "b"),
        ("DB_PORT", "5433abc"),
        ("DB_USER", ""),
        ("API_PORT", ""),
        ("API_TRUSTED_PROXY_CNT", "x"),
        ("RATE_LIMIT_ENABLE", "TRUE"),
        ("RATE_LIMIT_MAX", "7.9"),
        ("DB_SSL", "1"),
    ]))
    .unwrap();
    let db = config.db(Module::Brc20).unwrap();
    assert_eq!(db.port, 5433.0);
    assert_eq!(db.user, "postgres");
    assert!(!db.ssl);
    assert_eq!(config.api.port, 8000.0);
    assert!(config.api.trusted_proxies.is_nan());
    assert!(!config.rate_limit.enabled);
    assert_eq!(config.rate_limit.max, 7.0);
}

#[test]
fn the_snapshot_timeout_defaults_to_ten_minutes_and_must_be_positive() {
    let config = load_config(env(&[("DB_DATABASE", "b")])).unwrap();
    assert_eq!(
        config.snapshot_timeout().unwrap(),
        std::time::Duration::from_secs(600)
    );
    let config = load_config(env(&[
        ("DB_DATABASE", "b"),
        ("API_SNAPSHOT_TIMEOUT_MS", "250"),
    ]))
    .unwrap();
    assert_eq!(
        config.snapshot_timeout().unwrap(),
        std::time::Duration::from_millis(250)
    );
    for bad in ["0", "-5", "x"] {
        let config = load_config(env(&[
            ("DB_DATABASE", "b"),
            ("API_SNAPSHOT_TIMEOUT_MS", bad),
        ]))
        .unwrap();
        assert!(
            err(config.validate()).contains("API_SNAPSHOT_TIMEOUT_MS"),
            "{bad}"
        );
    }
}

#[test]
fn the_listen_port_must_be_a_port() {
    // node's listen() throws ERR_SOCKET_BAD_PORT for these
    for bad in ["x", "65536", "-1"] {
        let config = load_config(env(&[("DB_DATABASE", "b"), ("API_PORT", bad)])).unwrap();
        assert!(err(config.listen_port()).contains("API_PORT"), "{bad}");
    }
    let config = load_config(env(&[("DB_DATABASE", "b"), ("API_PORT", "0")])).unwrap();
    assert_eq!(config.listen_port().unwrap(), 0);
}

#[test]
fn trusted_proxies_count_hops_like_express() {
    // `trust proxy` = n trusts hop i when i < n; NaN and negatives trust none
    let hops = |value: &str| {
        load_config(env(&[
            ("DB_DATABASE", "b"),
            ("API_TRUSTED_PROXY_CNT", value),
        ]))
        .unwrap()
        .trusted_hops()
    };
    assert_eq!(hops("0"), 0);
    assert_eq!(hops("2"), 2);
    assert_eq!(hops("-3"), 0);
    assert_eq!(hops("x"), 0);
}

#[test]
fn db_connect_options_follow_node_pg() {
    let config = load_config(env(&[
        ("OPI_MODULES", "brc20,sns"),
        ("DB_DATABASE", "b"),
        ("DB_PORT", "0"),
        ("SNS_DB_DATABASE", "s"),
        ("SNS_DB_PORT", "70000"),
    ]))
    .unwrap();
    // node-pg treats a falsy port (0, NaN) as unset: PGPORT or 5432
    assert_eq!(config.db(Module::Brc20).unwrap().pg_port().unwrap(), None);
    assert!(err(config.db(Module::Sns).unwrap().pg_port()).contains("SNS_DB_PORT"));
}

#[test]
fn db_max_connections_must_be_positive() {
    for bad in ["0", "-1", "x"] {
        let config =
            load_config(env(&[("DB_DATABASE", "b"), ("DB_MAX_CONNECTIONS", bad)])).unwrap();
        assert!(
            err(config.db(Module::Brc20).unwrap().max_connections()).contains("DB_MAX_CONNECTIONS"),
            "{bad}"
        );
    }
}

#[test]
fn the_rate_limit_window_must_be_a_number() {
    let config = load_config(env(&[
        ("DB_DATABASE", "b"),
        ("RATE_LIMIT_ENABLE", "true"),
        ("RATE_LIMIT_WINDOW_MS", "soon"),
    ]))
    .unwrap();
    assert!(err(config.validate()).contains("RATE_LIMIT_WINDOW_MS"));
}

#[test]
fn opi_env_file_names_the_env_file_and_set_variables_win() {
    let dir = std::env::temp_dir().join(format!("opi-api-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("custom.env");
    std::fs::write(&file, "DB_DATABASE=from_file\nAPI_PORT=9001\n").unwrap();
    let vars = env_file_vars(Some(file.to_str().unwrap()), |key| key == "DB_DATABASE");
    assert_eq!(vars, [("API_PORT".to_string(), "9001".to_string())]);
    // a missing file adds nothing, as dotenv's config() does
    assert!(env_file_vars(Some(dir.join("missing.env").to_str().unwrap()), |_| false).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn modules_know_their_names_and_tables() {
    assert_eq!(
        MODULES.map(|m| (m.name(), m.block_hashes_table())),
        [
            ("brc20", "brc20_block_hashes"),
            ("bitmap", "bitmap_block_hashes"),
            ("sns", "sns_block_hashes"),
            ("runes", "runes_block_hashes"),
            ("inscriptions", "inscriptions_block_hashes"),
        ]
    );
}
