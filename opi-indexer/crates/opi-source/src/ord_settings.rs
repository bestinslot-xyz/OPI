use {
    anyhow::Result,
    clap::Parser,
    ord::{Chain, Options, settings::Settings},
    std::path::Path,
};

/// Builds the ord settings the indexer runs with: rune indexing on and a commit
/// interval so large that ord commits only when the index is closed. The index
/// file is `index` when given, else ord's default path under `data_dir`.
pub fn ord_settings(
    chain: Chain,
    data_dir: &Path,
    rpc_url: &str,
    index: Option<&Path>,
) -> Result<Settings> {
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
        "4294967295".into(),
    ];
    if let Some(index) = index {
        args.push("--index".into());
        args.push(index.to_string_lossy().into_owned());
    }
    let options = Options::try_parse_from(args)?;

    Settings::load(options)
}
