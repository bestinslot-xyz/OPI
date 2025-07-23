use {super::*, bitcoincore_rpc::Auth};

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
  bitcoin_data_dir: Option<PathBuf>,
  bitcoin_rpc_limit: Option<u32>,
  bitcoin_rpc_password: Option<String>,
  bitcoin_rpc_url: Option<String>,
  bitcoin_rpc_username: Option<String>,
  chain: Option<Chain>,
  commit_interval: Option<usize>,
  savepoint_interval: Option<usize>,
  max_savepoints: Option<usize>,
  config: Option<PathBuf>,
  config_dir: Option<PathBuf>,
  cookie_file: Option<PathBuf>,
  data_dir: Option<PathBuf>,
  height_limit: Option<u32>,
  index: Option<PathBuf>,
  index_cache_size: Option<usize>,
  integration_test: bool,
  no_index_inscriptions: bool,
}

impl Settings {
  pub fn load(options: Options) -> Result<Settings> {
    let mut env = BTreeMap::<String, String>::new();

    for (var, value) in env::vars_os() {
      let Some(var) = var.to_str() else {
        continue;
      };

      let Some(key) = var.strip_prefix("ORD_") else {
        continue;
      };

      env.insert(
        key.into(),
        value.into_string().map_err(|value| {
          anyhow!(
            "environment variable `{var}` not valid unicode: `{}`",
            value.to_string_lossy()
          )
        })?,
      );
    }

    Self::merge(options, env)
  }

  pub fn merge(options: Options, env: BTreeMap<String, String>) -> Result<Self> {
    let settings = Settings::from_options(options).or(Settings::from_env(env)?);

    let config_path = if let Some(path) = &settings.config {
      Some(path.into())
    } else {
      let path = if let Some(dir) = settings.config_dir.clone().or(settings.data_dir.clone()) {
        dir
      } else {
        Self::default_data_dir()?
      }
      .join("ord.yaml");

      path.exists().then_some(path)
    };

    let config = if let Some(config_path) = config_path {
      serde_yaml::from_reader(File::open(&config_path).context(anyhow!(
        "failed to open config file `{}`",
        config_path.display()
      ))?)
      .context(anyhow!(
        "failed to deserialize config file `{}`",
        config_path.display()
      ))?
    } else {
      Settings::default()
    };

    let settings = settings.or(config).or_defaults()?;

    match (
      &settings.bitcoin_rpc_username,
      &settings.bitcoin_rpc_password,
    ) {
      (None, Some(_rpc_pass)) => bail!("no bitcoin RPC username specified"),
      (Some(_rpc_user), None) => bail!("no bitcoin RPC password specified"),
      _ => {}
    };

    Ok(settings)
  }

  pub fn or(self, source: Settings) -> Self {
    Self {
      bitcoin_data_dir: self.bitcoin_data_dir.or(source.bitcoin_data_dir),
      bitcoin_rpc_limit: self.bitcoin_rpc_limit.or(source.bitcoin_rpc_limit),
      bitcoin_rpc_password: self.bitcoin_rpc_password.or(source.bitcoin_rpc_password),
      bitcoin_rpc_url: self.bitcoin_rpc_url.or(source.bitcoin_rpc_url),
      bitcoin_rpc_username: self.bitcoin_rpc_username.or(source.bitcoin_rpc_username),
      chain: self.chain.or(source.chain),
      commit_interval: self.commit_interval.or(source.commit_interval),
      savepoint_interval: self.savepoint_interval.or(source.savepoint_interval),
      max_savepoints: self.max_savepoints.or(source.max_savepoints),
      config: self.config.or(source.config),
      config_dir: self.config_dir.or(source.config_dir),
      cookie_file: self.cookie_file.or(source.cookie_file),
      data_dir: self.data_dir.or(source.data_dir),
      height_limit: self.height_limit.or(source.height_limit),
      index: self.index.or(source.index),
      index_cache_size: self.index_cache_size.or(source.index_cache_size),
      integration_test: self.integration_test || source.integration_test,
      no_index_inscriptions: self.no_index_inscriptions || source.no_index_inscriptions,
    }
  }

  pub fn from_options(options: Options) -> Self {
    Self {
      bitcoin_data_dir: options.bitcoin_data_dir,
      bitcoin_rpc_limit: options.bitcoin_rpc_limit,
      bitcoin_rpc_password: options.bitcoin_rpc_password,
      bitcoin_rpc_url: options.bitcoin_rpc_url,
      bitcoin_rpc_username: options.bitcoin_rpc_username,
      chain: options
        .signet
        .then_some(Chain::Signet)
        .or(options.regtest.then_some(Chain::Regtest))
        .or(options.testnet.then_some(Chain::Testnet))
        .or(options.testnet4.then_some(Chain::Testnet4))
        .or(options.chain_argument),
      commit_interval: options.commit_interval,
      savepoint_interval: options.savepoint_interval,
      max_savepoints: options.max_savepoints,
      config: options.config,
      config_dir: options.config_dir,
      cookie_file: options.cookie_file,
      data_dir: options.data_dir,
      height_limit: options.height_limit,
      index: options.index,
      index_cache_size: options.index_cache_size,
      integration_test: options.integration_test,
      no_index_inscriptions: options.no_index_inscriptions,
    }
  }

  pub fn from_env(env: BTreeMap<String, String>) -> Result<Self> {
    let get_bool = |key| {
      env
        .get(key)
        .map(|value| !value.is_empty())
        .unwrap_or_default()
    };

    let get_string = |key| env.get(key).cloned();

    let get_path = |key| env.get(key).map(PathBuf::from);

    let get_chain = |key| {
      env
        .get(key)
        .map(|chain| chain.parse::<Chain>())
        .transpose()
        .with_context(|| format!("failed to parse environment variable ORD_{key} as chain"))
    };

    let get_u32 = |key| {
      env
        .get(key)
        .map(|int| int.parse::<u32>())
        .transpose()
        .with_context(|| format!("failed to parse environment variable ORD_{key} as u32"))
    };

    let get_usize = |key| {
      env
        .get(key)
        .map(|int| int.parse::<usize>())
        .transpose()
        .with_context(|| format!("failed to parse environment variable ORD_{key} as usize"))
    };

    Ok(Self {
      bitcoin_data_dir: get_path("BITCOIN_DATA_DIR"),
      bitcoin_rpc_limit: get_u32("BITCOIN_RPC_LIMIT")?,
      bitcoin_rpc_password: get_string("BITCOIN_RPC_PASSWORD"),
      bitcoin_rpc_url: get_string("BITCOIN_RPC_URL"),
      bitcoin_rpc_username: get_string("BITCOIN_RPC_USERNAME"),
      chain: get_chain("CHAIN")?,
      commit_interval: get_usize("COMMIT_INTERVAL")?,
      savepoint_interval: get_usize("SAVEPOINT_INTERVAL")?,
      max_savepoints: get_usize("MAX_SAVEPOINTS")?,
      config: get_path("CONFIG"),
      config_dir: get_path("CONFIG_DIR"),
      cookie_file: get_path("COOKIE_FILE"),
      data_dir: get_path("DATA_DIR"),
      height_limit: get_u32("HEIGHT_LIMIT")?,
      index: get_path("INDEX"),
      index_cache_size: get_usize("INDEX_CACHE_SIZE")?,
      integration_test: get_bool("INTEGRATION_TEST"),
      no_index_inscriptions: get_bool("NO_INDEX_INSCRIPTIONS"),
    })
  }

  pub fn for_env(dir: &Path, rpc_url: &str, _server_url: &str) -> Self {
    Self {
      bitcoin_data_dir: Some(dir.into()),
      bitcoin_rpc_password: None,
      bitcoin_rpc_url: Some(rpc_url.into()),
      bitcoin_rpc_username: None,
      bitcoin_rpc_limit: None,
      chain: Some(Chain::Regtest),
      commit_interval: None,
      savepoint_interval: None,
      max_savepoints: None,
      config: None,
      config_dir: None,
      cookie_file: None,
      data_dir: Some(dir.into()),
      height_limit: None,
      index: None,
      index_cache_size: None,
      integration_test: false,
      no_index_inscriptions: false,
    }
  }

  pub fn or_defaults(self) -> Result<Self> {
    let chain = self.chain.unwrap_or_default();

    let bitcoin_data_dir = match &self.bitcoin_data_dir {
      Some(bitcoin_data_dir) => bitcoin_data_dir.clone(),
      None => {
        if cfg!(target_os = "linux") {
          dirs::home_dir()
            .ok_or_else(|| anyhow!("failed to get cookie file path: could not get home dir"))?
            .join(".bitcoin")
        } else {
          dirs::data_dir()
            .ok_or_else(|| anyhow!("failed to get cookie file path: could not get data dir"))?
            .join("Bitcoin")
        }
      }
    };

    let cookie_file = match self.cookie_file {
      Some(cookie_file) => cookie_file,
      None => chain.join_with_data_dir(&bitcoin_data_dir).join(".cookie"),
    };

    let data_dir = chain.join_with_data_dir(match &self.data_dir {
      Some(data_dir) => data_dir.clone(),
      None => Self::default_data_dir()?,
    });

    let index = match &self.index {
      Some(path) => path.clone(),
      None => data_dir.join("dbs"),
    };

    Ok(Self {
      bitcoin_data_dir: Some(bitcoin_data_dir),
      bitcoin_rpc_limit: Some(self.bitcoin_rpc_limit.unwrap_or(12)),
      bitcoin_rpc_password: self.bitcoin_rpc_password,
      bitcoin_rpc_url: Some(
        self
          .bitcoin_rpc_url
          .clone()
          .unwrap_or_else(|| format!("127.0.0.1:{}", chain.default_rpc_port())),
      ),
      bitcoin_rpc_username: self.bitcoin_rpc_username,
      chain: Some(chain),
      commit_interval: Some(self.commit_interval.unwrap_or(5000)),
      savepoint_interval: Some(self.savepoint_interval.unwrap_or(10)),
      max_savepoints: Some(self.max_savepoints.unwrap_or(2)),
      config: None,
      config_dir: None,
      cookie_file: Some(cookie_file),
      data_dir: Some(data_dir),
      height_limit: self.height_limit,
      index: Some(index),
      index_cache_size: Some(match self.index_cache_size {
        Some(index_cache_size) => index_cache_size,
        None => {
          let mut sys = System::new();
          sys.refresh_memory();
          usize::try_from(sys.total_memory() / 4)?
        }
      }),
      integration_test: self.integration_test,
      no_index_inscriptions: self.no_index_inscriptions,
    })
  }

  pub fn default_data_dir() -> Result<PathBuf> {
    Ok(
      dirs::data_dir()
        .context("could not get data dir")?
        .join("ord"),
    )
  }

  pub fn bitcoin_credentials(&self) -> Result<Auth> {
    if let Some((user, pass)) = &self
      .bitcoin_rpc_username
      .as_ref()
      .zip(self.bitcoin_rpc_password.as_ref())
    {
      Ok(Auth::UserPass((*user).clone(), (*pass).clone()))
    } else {
      Ok(Auth::CookieFile(self.cookie_file()?))
    }
  }

  pub fn bitcoin_rpc_client(&self, wallet: Option<String>) -> Result<Client> {
    let rpc_url = self.bitcoin_rpc_url(wallet);

    let bitcoin_credentials = self.bitcoin_credentials()?;

    log::trace!(
      "Connecting to Bitcoin Core at {}",
      self.bitcoin_rpc_url(None)
    );

    if let Auth::CookieFile(cookie_file) = &bitcoin_credentials {
      log::trace!(
        "Using credentials from cookie file at `{}`",
        cookie_file.display()
      );

      ensure!(
        cookie_file.is_file(),
        "cookie file `{}` does not exist",
        cookie_file.display()
      );
    }

    let client = Client::new(&rpc_url, bitcoin_credentials.clone()).with_context(|| {
      format!(
        "failed to connect to Bitcoin Core RPC at `{rpc_url}` with {}",
        match bitcoin_credentials {
          Auth::None => "no credentials".into(),
          Auth::UserPass(_, _) => "username and password".into(),
          Auth::CookieFile(cookie_file) => format!("cookie file at {}", cookie_file.display()),
        }
      )
    })?;

    let mut checks = 0;
    let rpc_chain = loop {
      match client.get_blockchain_info() {
        Ok(blockchain_info) => {
          break match blockchain_info.chain.to_string().as_str() {
            "bitcoin" => Chain::Mainnet,
            "regtest" => Chain::Regtest,
            "signet" => Chain::Signet,
            "testnet" => Chain::Testnet,
            "testnet4" => Chain::Testnet4,
            other => bail!("Bitcoin RPC server on unknown chain: {other}"),
          }
        }
        Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::Error::Rpc(err)))
          if err.code == -28 => {}
        Err(err) if err.to_string().contains("Resource temporarily unavailable") => {}
        Err(err) => bail!("Failed to connect to Bitcoin Core RPC at `{rpc_url}`:  {err}"),
      }

      ensure! {
        checks < 100,
        "Failed to connect to Bitcoin Core RPC at `{rpc_url}`",
      }

      checks += 1;
      thread::sleep(Duration::from_millis(100));
    };

    let ord_chain = self.chain();

    if rpc_chain != ord_chain {
      bail!("Bitcoin RPC server is on {rpc_chain} but ord is on {ord_chain}");
    }

    Ok(client)
  }

  pub fn chain(&self) -> Chain {
    self.chain.unwrap()
  }

  pub fn commit_interval(&self) -> usize {
    self.commit_interval.unwrap()
  }

  pub fn savepoint_interval(&self) -> usize {
    self.savepoint_interval.unwrap()
  }

  pub fn max_savepoints(&self) -> usize {
    self.max_savepoints.unwrap()
  }

  pub fn cookie_file(&self) -> Result<PathBuf> {
    if let Some(cookie_file) = &self.cookie_file {
      return Ok(cookie_file.clone());
    }

    let path = if let Some(bitcoin_data_dir) = &self.bitcoin_data_dir {
      bitcoin_data_dir.clone()
    } else if cfg!(target_os = "linux") {
      dirs::home_dir()
        .ok_or_else(|| anyhow!("failed to get cookie file path: could not get home dir"))?
        .join(".bitcoin")
    } else {
      dirs::data_dir()
        .ok_or_else(|| anyhow!("failed to get cookie file path: could not get data dir"))?
        .join("Bitcoin")
    };

    let path = self.chain().join_with_data_dir(path);

    Ok(path.join(".cookie"))
  }

  pub fn data_dir(&self) -> PathBuf {
    self.data_dir.as_ref().unwrap().into()
  }

  pub fn first_inscription_height(&self) -> u32 {
    if self.integration_test {
      0
    } else {
      self.chain.unwrap().first_inscription_height()
    }
  }

  pub fn first_rune_height(&self) -> u32 {
    if self.integration_test {
      0
    } else {
      self.chain.unwrap().first_rune_height()
    }
  }

  pub fn height_limit(&self) -> Option<u32> {
    self.height_limit
  }

  pub fn index(&self) -> &Path {
    self.index.as_ref().unwrap()
  }

  pub fn index_inscriptions_raw(&self) -> bool {
    !self.no_index_inscriptions
  }

  pub fn index_cache_size(&self) -> usize {
    self.index_cache_size.unwrap()
  }

  pub fn integration_test(&self) -> bool {
    self.integration_test
  }

  pub fn bitcoin_rpc_url(&self, wallet_name: Option<String>) -> String {
    let base_url = self.bitcoin_rpc_url.as_ref().unwrap();
    match wallet_name {
      Some(wallet_name) => format!("{base_url}/wallet/{wallet_name}"),
      None => format!("{base_url}/"),
    }
  }

  pub fn bitcoin_rpc_limit(&self) -> u32 {
    self.bitcoin_rpc_limit.unwrap()
  }
}
