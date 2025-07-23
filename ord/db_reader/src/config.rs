use bitcoin::Network;
use std::path::PathBuf;

pub struct Config {
  pub network: Network,
  pub db_path: Option<PathBuf>,
  pub api_url: Option<String>,
}
