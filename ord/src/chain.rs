use {super::*, clap::ValueEnum};

#[derive(Default, ValueEnum, Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chain {
  #[default]
  #[value(alias("main"))]
  Mainnet,
  #[value(alias("test"))]
  Testnet,
  #[value(alias("testnet4"))]
  Testnet4,
  Signet,
  Regtest,
}

impl Chain {
  pub(crate) fn network(self) -> Network {
    match self {
      Self::Mainnet => Network::Bitcoin,
      Self::Testnet => Network::Testnet,
      Self::Testnet4 => Network::Testnet,
      Self::Signet => Network::Signet,
      Self::Regtest => Network::Regtest,
    }
  }

  pub(crate) fn default_rpc_port(self) -> u16 {
    match self {
      Self::Mainnet => 8332,
      Self::Regtest => 18443,
      Self::Signet => 38332,
      Self::Testnet => 18332,
      Self::Testnet4 => 48332,
    }
  }

  pub(crate) fn inscription_content_size_limit(self) -> Option<usize> {
    match self {
      Self::Mainnet | Self::Regtest => None,
      Self::Testnet | Self::Testnet4 | Self::Signet => Some(1024),
    }
  }

  pub(crate) fn first_inscription_height(self) -> u32 {
    match self {
      Self::Mainnet => 767430,
      Self::Regtest => 0,
      Self::Signet => 112402,
      Self::Testnet => 2413343,
      Self::Testnet4 => 0,
    }
  }

  pub(crate) fn first_rune_height(self) -> u32 {
    SUBSIDY_HALVING_INTERVAL
      * match self {
        Self::Mainnet => 4,
        Self::Regtest => 0,
        Self::Signet => 0,
        Self::Testnet => 12,
        Self::Testnet4 => 0,
      }
  }

  pub(crate) fn jubilee_height(self) -> u32 {
    match self {
      Self::Mainnet => 824544,
      Self::Regtest => 110,
      Self::Signet => 175392,
      Self::Testnet => 2544192,
      Self::Testnet4 => 0,
    }
  }

  pub(crate) fn genesis_block(self) -> Block {
    bitcoin::blockdata::constants::genesis_block(self.network())
  }

  pub(crate) fn address_from_script(
    self,
    script: &Script,
  ) -> Result<Address, bitcoin::address::Error> {
    Address::from_script(script, self.network())
  }

  pub(crate) fn join_with_data_dir(self, data_dir: &Path) -> PathBuf {
    match self {
      Self::Mainnet => data_dir.to_owned(),
      Self::Testnet => data_dir.join("testnet3"),
      Self::Testnet4 => data_dir.join("testnet4"),
      Self::Signet => data_dir.join("signet"),
      Self::Regtest => data_dir.join("regtest"),
    }
  }
}

impl Display for Chain {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    write!(
      f,
      "{}",
      match self {
        Self::Mainnet => "mainnet",
        Self::Regtest => "regtest",
        Self::Signet => "signet",
        Self::Testnet => "testnet",
        Self::Testnet4 => "testnet4",
      }
    )
  }
}
