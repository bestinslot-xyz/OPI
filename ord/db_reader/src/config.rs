use bitcoin::Network;

pub struct Config {
  pub network: Network,
  pub api_url: Option<String>,
}
