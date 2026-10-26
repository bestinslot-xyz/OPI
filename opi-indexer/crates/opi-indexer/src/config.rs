//! Process configuration read from `OPI_*` and `BITCOIN_RPC_*` variables.

use {
    anyhow::{Context, Result, anyhow, bail},
    opi_source::{
        driver::{DEFAULT_CHUNK, SourceConfig},
        proxy::UpstreamAuth,
    },
    ord::Chain,
    std::path::PathBuf,
};

#[derive(Debug, Clone)]
pub struct Config {
    pub source: SourceConfig,
    pub modules: Vec<ModuleKind>,
}

impl Config {
    pub fn from_env() -> Result<Config> {
        Config::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config> {
        let required = |key: &str| {
            get(key)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("{key} is not set"))
        };

        let chain = parse_chain(&required("OPI_CHAIN")?)?;
        let bitcoin_auth = match (
            get("BITCOIN_RPC_COOKIE").filter(|value| !value.is_empty()),
            get("BITCOIN_RPC_USER").filter(|value| !value.is_empty()),
            get("BITCOIN_RPC_PASSWD"),
        ) {
            (Some(cookie), None, _) => UpstreamAuth::Cookie(PathBuf::from(cookie)),
            (None, Some(user), Some(passwd)) => UpstreamAuth::UserPass(user, passwd),
            (None, Some(_), None) => bail!("BITCOIN_RPC_USER is set without BITCOIN_RPC_PASSWD"),
            (Some(_), Some(_), _) => {
                bail!("set either BITCOIN_RPC_COOKIE or BITCOIN_RPC_USER, not both")
            }
            (None, None, _) => {
                bail!("set BITCOIN_RPC_COOKIE or BITCOIN_RPC_USER and BITCOIN_RPC_PASSWD")
            }
        };
        let chunk = match get("OPI_CHUNK").filter(|value| !value.is_empty()) {
            Some(value) => value
                .parse::<u32>()
                .ok()
                .filter(|chunk| *chunk > 0)
                .with_context(|| format!("OPI_CHUNK must be a positive integer, got `{value}`"))?,
            None => DEFAULT_CHUNK,
        };

        let modules = parse_modules(&get("OPI_MODULES").unwrap_or_else(|| "brc20".into()))?;

        Ok(Config {
            source: SourceConfig {
                chain,
                ord_data_dir: PathBuf::from(required("OPI_ORD_DATA_DIR")?),
                journal_dir: PathBuf::from(required("OPI_JOURNAL_DIR")?),
                bitcoin_rpc_url: required("BITCOIN_RPC_URL")?,
                bitcoin_auth,
                chunk,
            },
            modules,
        })
    }

    /// The `bitcoin` network matching the configured chain.
    pub fn network(&self) -> bitcoin::Network {
        network_of(self.source.chain)
    }

    /// The `NETWORK_TYPE` value naming the configured chain.
    pub fn network_name(&self) -> &'static str {
        match self.source.chain {
            Chain::Mainnet => "mainnet",
            Chain::Signet => "signet",
            Chain::Testnet => "testnet",
            Chain::Testnet4 => "testnet4",
            Chain::Regtest => "regtest",
        }
    }

    /// Fails when a module's `NETWORK_TYPE` names another network than
    /// `OPI_CHAIN`.
    pub fn check_network(&self, module_network: &str) -> Result<()> {
        if module_network != self.network_name() {
            bail!(
                "NETWORK_TYPE {module_network:?} does not match OPI_CHAIN network {:?}",
                self.network_name()
            );
        }
        Ok(())
    }
}

/// A module `OPI_MODULES` can enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Brc20,
    Bitmap,
    Sns,
}

/// Parses a comma-separated module list, trimming names and dropping repeats.
fn parse_modules(value: &str) -> Result<Vec<ModuleKind>> {
    let mut modules = Vec::new();
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let module = match name {
            "brc20" => ModuleKind::Brc20,
            "bitmap" => ModuleKind::Bitmap,
            "sns" => ModuleKind::Sns,
            other => {
                bail!("unknown module `{other}` in OPI_MODULES (supported: brc20, bitmap, sns)")
            }
        };
        if !modules.contains(&module) {
            modules.push(module);
        }
    }
    if modules.is_empty() {
        bail!("OPI_MODULES names no module (supported: brc20, bitmap, sns)");
    }
    Ok(modules)
}

fn parse_chain(value: &str) -> Result<Chain> {
    match value {
        "mainnet" => Ok(Chain::Mainnet),
        "signet" => Ok(Chain::Signet),
        "testnet4" => Ok(Chain::Testnet4),
        "regtest" => Ok(Chain::Regtest),
        other => bail!("OPI_CHAIN must be mainnet, signet, testnet4 or regtest, got `{other}`"),
    }
}

fn network_of(chain: Chain) -> bitcoin::Network {
    match chain {
        Chain::Mainnet => bitcoin::Network::Bitcoin,
        Chain::Signet => bitcoin::Network::Signet,
        Chain::Testnet => bitcoin::Network::Testnet,
        Chain::Testnet4 => bitcoin::Network::Testnet4,
        Chain::Regtest => bitcoin::Network::Regtest,
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::collections::HashMap};

    fn base() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("OPI_CHAIN", "signet"),
            ("OPI_ORD_DATA_DIR", "/data/ord"),
            ("OPI_JOURNAL_DIR", "/data/journal"),
            ("BITCOIN_RPC_URL", "http://127.0.0.1:38332"),
            ("BITCOIN_RPC_USER", "user"),
            ("BITCOIN_RPC_PASSWD", "pass"),
        ])
    }

    fn load(vars: &HashMap<&'static str, &'static str>) -> Result<Config> {
        Config::from_lookup(|key| vars.get(key).map(|value| value.to_string()))
    }

    #[test]
    fn config_from_env_parses_defaults() {
        let config = load(&base()).unwrap();
        assert_eq!(config.source.chunk, 1000);
        assert_eq!(config.modules, vec![ModuleKind::Brc20]);
        assert_eq!(config.source.chain, Chain::Signet);
        assert_eq!(config.network(), bitcoin::Network::Signet);
        assert_eq!(config.network_name(), "signet");
        assert_eq!(config.source.ord_data_dir, PathBuf::from("/data/ord"));
        assert!(matches!(
            config.source.bitcoin_auth,
            UpstreamAuth::UserPass(ref user, ref pass) if user == "user" && pass == "pass"
        ));

        let mut vars = base();
        vars.insert("OPI_CHUNK", "250");
        assert_eq!(load(&vars).unwrap().source.chunk, 250);
    }

    #[test]
    fn config_parses_all_modules() {
        let mut vars = base();
        vars.insert("OPI_MODULES", "brc20, bitmap ,sns,bitmap");
        assert_eq!(
            load(&vars).unwrap().modules,
            vec![ModuleKind::Brc20, ModuleKind::Bitmap, ModuleKind::Sns]
        );

        vars.insert("OPI_MODULES", "sns");
        assert_eq!(load(&vars).unwrap().modules, vec![ModuleKind::Sns]);

        vars.insert("OPI_MODULES", " , ");
        assert!(load(&vars).is_err());
        vars.insert("OPI_MODULES", "");
        assert!(load(&vars).is_err());
    }

    #[test]
    fn unknown_module_is_rejected() {
        let mut vars = base();
        vars.insert("OPI_MODULES", "brc20,runes");
        let err = load(&vars).unwrap_err();
        assert!(err.to_string().contains("runes"), "{err}");
    }

    #[test]
    fn module_env_prefixes_are_independent() {
        // No other test in this binary reads these variables.
        unsafe {
            std::env::set_var("BITMAP_DB_DATABASE", "bitmap_db");
            std::env::set_var("SNS_DB_DATABASE", "sns_db");
        }
        let bitmap = opi_bitmap::BitmapConfig::from_env().unwrap();
        let sns = opi_sns::SnsConfig::from_env().unwrap();
        assert_eq!(bitmap.db_database, "bitmap_db");
        assert_eq!(sns.db_database, "sns_db");
    }

    #[test]
    fn module_network_must_match_the_chain() {
        let config = load(&base()).unwrap();
        assert!(config.check_network("signet").is_ok());
        let err = config.check_network("mainnet").unwrap_err();
        assert!(err.to_string().contains("mainnet"), "{err}");
    }

    #[test]
    fn cookie_auth_and_invalid_values_are_rejected() {
        let mut vars = base();
        vars.remove("BITCOIN_RPC_USER");
        vars.remove("BITCOIN_RPC_PASSWD");
        assert!(load(&vars).is_err());
        vars.insert("BITCOIN_RPC_COOKIE", "/data/.cookie");
        assert!(matches!(
            load(&vars).unwrap().source.bitcoin_auth,
            UpstreamAuth::Cookie(_)
        ));

        let mut vars = base();
        vars.insert("OPI_CHAIN", "testnet");
        assert!(load(&vars).is_err());
        let mut vars = base();
        vars.insert("OPI_CHUNK", "0");
        assert!(load(&vars).is_err());
        let mut vars = base();
        vars.remove("OPI_JOURNAL_DIR");
        assert!(load(&vars).is_err());
    }
}
