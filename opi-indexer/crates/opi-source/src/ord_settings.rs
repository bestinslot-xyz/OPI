use {
  anyhow::Result,
  clap::Parser,
  ord::{Chain, Options, settings::Settings},
  std::path::Path,
};

/// Builds the ord settings the indexer runs with: rune indexing on and a commit
/// interval so large that ord commits only when the index is closed.
pub fn ord_settings(chain: Chain, data_dir: &Path, rpc_url: &str) -> Result<Settings> {
  let options = Options::try_parse_from([
    "ord",
    "--chain",
    &chain.to_string(),
    "--data-dir",
    &data_dir.to_string_lossy(),
    "--bitcoin-rpc-url",
    rpc_url,
    "--bitcoin-rpc-username",
    "opi",
    "--bitcoin-rpc-password",
    "opi",
    "--index-runes",
    "--commit-interval",
    "4294967295",
  ])?;

  Settings::load(options)
}
