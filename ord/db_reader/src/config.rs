use std::path::PathBuf;
use bitcoin::Network;

pub struct Config {
    pub network: Network,
    pub db_path: Option<PathBuf>,
    pub api_url: Option<String>,
}