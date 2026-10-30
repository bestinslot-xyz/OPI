use {
    anyhow::{Result, ensure},
    clap::Parser,
    ord::{Chain, Index, Options, settings::Settings},
    std::path::Path,
};

/// ord's commit interval: so large that ord commits only at a savepoint
/// barrier or when `update()` returns.
pub const COMMIT_INTERVAL: usize = u32::MAX as usize;

/// Default size of the redb page cache of ord's index, in bytes. ord reads
/// its index through the kernel's page cache, so redb's own cache holds a
/// second copy of each page it keeps; a small one leaves that memory to the
/// page cache.
pub const DEFAULT_ORD_CACHE_BYTES: usize = 1024 * 1024 * 1024;

/// Builds the ord settings the indexer runs with: rune indexing when `runes`
/// is set, a commit interval of [`COMMIT_INTERVAL`] and a page cache of
/// `cache_bytes`. The index file is `index` when given, else ord's default
/// path under `data_dir`.
pub fn ord_settings(
    chain: Chain,
    data_dir: &Path,
    rpc_url: &str,
    index: Option<&Path>,
    runes: bool,
    cache_bytes: usize,
) -> Result<Settings> {
    Settings::load(options(
        chain,
        data_dir,
        rpc_url,
        index,
        runes,
        cache_bytes,
    )?)
}

fn options(
    chain: Chain,
    data_dir: &Path,
    rpc_url: &str,
    index: Option<&Path>,
    runes: bool,
    cache_bytes: usize,
) -> Result<Options> {
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
        "--commit-interval".into(),
        COMMIT_INTERVAL.to_string(),
        "--index-cache-size".into(),
        cache_bytes.to_string(),
    ];
    if runes {
        args.push("--index-runes".into());
    }
    if let Some(index) = index {
        args.push("--index".into());
        args.push(index.to_string_lossy().into_owned());
    }
    Ok(Options::try_parse_from(args)?)
}

/// Refuses ord settings that break the commit barrier: ord commits a block
/// only at a savepoint barrier, which `integration_test` switches off, or at
/// the end of `update()`, which any commit interval below [`COMMIT_INTERVAL`]
/// pre-empts. Also refuses rune indexing that differs from `runes`: the
/// journal's rune records follow the same switch. `ORD_INTEGRATION_TEST`,
/// `ORD_INDEX_RUNES` and an `ord.yaml` in the data dir all reach these
/// settings.
pub fn check_ord_settings(settings: &Settings, runes: bool) -> Result<()> {
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
    ensure!(
        settings.index_runes_raw() == runes,
        "ord's index_runes setting is on (ORD_INDEX_RUNES or ord.yaml), but runes is disabled; \
         rune indexing follows the enabled modules"
    );
    Ok(())
}

/// Refuses an ord index whose rune indexing differs from `runes`. ord records
/// the setting when it creates the index and keeps it on every later open,
/// so switching needs a new ord data dir and journal.
pub fn check_ord_index(index: &Index, data_dir: &Path, runes: bool) -> Result<()> {
    let built = index.has_rune_index();
    ensure!(
        built == runes,
        "ord's index was built {} rune indexing, but runes is {}; start with runes {} again, \
         or delete ord's data dir {} and the journal and index again",
        with_or_without(built),
        enabled_or_disabled(runes),
        enabled_or_disabled(built),
        data_dir.display()
    );
    Ok(())
}

pub(crate) fn with_or_without(runes: bool) -> &'static str {
    if runes { "with" } else { "without" }
}

pub(crate) fn enabled_or_disabled(runes: bool) -> &'static str {
    if runes { "enabled" } else { "disabled" }
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

    fn indexer_options(dir: &Path, runes: bool) -> Options {
        options(
            Chain::Regtest,
            dir,
            "http://127.0.0.1:1",
            None,
            runes,
            DEFAULT_ORD_CACHE_BYTES,
        )
        .unwrap()
    }

    #[test]
    fn the_page_cache_size_overrides_ords_environment() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(
            Chain::Regtest,
            dir.path(),
            "http://127.0.0.1:1",
            None,
            false,
            64 << 20,
        )
        .unwrap();
        let settings = merged(options, &[("INDEX_CACHE_SIZE", "17179869184")]);
        assert_eq!(settings.index_cache_size(), 64 << 20);
    }

    #[test]
    fn accepts_the_indexer_settings() {
        let dir = tempfile::tempdir().unwrap();
        for runes in [true, false] {
            check_ord_settings(&merged(indexer_options(dir.path(), runes), &[]), runes).unwrap();
        }
    }

    #[test]
    fn rune_indexing_follows_the_runes_switch() {
        let dir = tempfile::tempdir().unwrap();
        for runes in [true, false] {
            let settings = merged(indexer_options(dir.path(), runes), &[]);
            assert_eq!(settings.index_runes_raw(), runes);
        }
    }

    #[test]
    fn refuses_rune_indexing_switched_on_outside_the_indexer() {
        let dir = tempfile::tempdir().unwrap();
        let settings = merged(indexer_options(dir.path(), false), &[("INDEX_RUNES", "1")]);
        let err = check_ord_settings(&settings, false).unwrap_err();
        assert!(err.to_string().contains("ORD_INDEX_RUNES"), "{err:#}");
    }

    #[test]
    fn refuses_integration_test() {
        let dir = tempfile::tempdir().unwrap();
        let settings = merged(
            indexer_options(dir.path(), true),
            &[("INTEGRATION_TEST", "1")],
        );
        let err = check_ord_settings(&settings, true).unwrap_err();
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
        let err = check_ord_settings(&merged(options, &[]), false).unwrap_err();
        assert!(err.to_string().contains("commit interval"), "{err:#}");
    }
}
