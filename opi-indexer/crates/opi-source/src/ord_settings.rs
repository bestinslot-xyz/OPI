use {
    anyhow::{Result, ensure},
    clap::Parser,
    ord::{Chain, Options, settings::Settings},
    std::path::Path,
};

/// ord's commit interval: so large that ord commits only at a savepoint
/// barrier or when `update()` returns.
pub const COMMIT_INTERVAL: usize = u32::MAX as usize;

/// Builds the ord settings the indexer runs with: rune indexing on and a commit
/// interval of [`COMMIT_INTERVAL`]. The index file is `index` when given, else
/// ord's default path under `data_dir`.
pub fn ord_settings(
    chain: Chain,
    data_dir: &Path,
    rpc_url: &str,
    index: Option<&Path>,
) -> Result<Settings> {
    Settings::load(options(chain, data_dir, rpc_url, index)?)
}

fn options(chain: Chain, data_dir: &Path, rpc_url: &str, index: Option<&Path>) -> Result<Options> {
    let mut args = vec![
        "ord".to_string(),
        "--chain".into(),
        chain.to_string(),
        "--data-dir".into(),
        data_dir.to_string_lossy().into_owned(),
        "--bitcoin-rpc-url".into(),
        rpc_url.into(),
        "--bitcoin-rpc-username".into(),
        "opi".into(),
        "--bitcoin-rpc-password".into(),
        "opi".into(),
        "--index-runes".into(),
        "--commit-interval".into(),
        COMMIT_INTERVAL.to_string(),
    ];
    if let Some(index) = index {
        args.push("--index".into());
        args.push(index.to_string_lossy().into_owned());
    }
    Ok(Options::try_parse_from(args)?)
}

/// Refuses ord settings that break the commit barrier: ord commits a block
/// only at a savepoint barrier, which `integration_test` switches off, or at
/// the end of `update()`, which any commit interval below [`COMMIT_INTERVAL`]
/// pre-empts. `ORD_INTEGRATION_TEST` and an `ord.yaml` in the data dir both
/// reach these settings.
pub fn check_ord_settings(settings: &Settings) -> Result<()> {
    ensure!(
        !settings.integration_test(),
        "ord's integration_test setting is on (ORD_INTEGRATION_TEST or ord.yaml); it disables \
         the savepoint commit barrier the journal relies on"
    );
    ensure!(
        settings.commit_interval() == COMMIT_INTERVAL,
        "ord's commit interval is {}, expected {COMMIT_INTERVAL}; the journal relies on ord \
         committing only at savepoint barriers and the end of update()",
        settings.commit_interval()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, std::collections::BTreeMap};

    /// Merges `options` with an `ORD_*` environment given as `env`, so the
    /// test doesn't touch the process environment.
    fn merged(options: Options, env: &[(&str, &str)]) -> Settings {
        let env = env
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>();
        Settings::merge(options, env).unwrap()
    }

    fn indexer_options(dir: &Path) -> Options {
        options(Chain::Regtest, dir, "http://127.0.0.1:1", None).unwrap()
    }

    #[test]
    fn accepts_the_indexer_settings() {
        let dir = tempfile::tempdir().unwrap();
        check_ord_settings(&merged(indexer_options(dir.path()), &[])).unwrap();
    }

    #[test]
    fn refuses_integration_test() {
        let dir = tempfile::tempdir().unwrap();
        let settings = merged(indexer_options(dir.path()), &[("INTEGRATION_TEST", "1")]);
        let err = check_ord_settings(&settings).unwrap_err();
        assert!(err.to_string().contains("integration_test"), "{err:#}");
    }

    #[test]
    fn refuses_another_commit_interval() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().into_owned();
        let options = Options::try_parse_from([
            "ord",
            "--chain",
            "regtest",
            "--data-dir",
            &data_dir,
            "--commit-interval",
            "5000",
        ])
        .unwrap();
        let err = check_ord_settings(&merged(options, &[])).unwrap_err();
        assert!(err.to_string().contains("commit interval"), "{err:#}");
    }
}
