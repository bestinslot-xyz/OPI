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
    pub brc20_enabled: bool,
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

        let mut brc20_enabled = false;
        let modules = get("OPI_MODULES").unwrap_or_else(|| "brc20".into());
        for module in modules
            .split(',')
            .map(str::trim)
            .filter(|module| !module.is_empty())
        {
            match module {
                "brc20" => brc20_enabled = true,
                other => bail!("unknown module `{other}` in OPI_MODULES (supported: brc20)"),
            }
        }

        Ok(Config {
            source: SourceConfig {
                chain,
                ord_data_dir: PathBuf::from(required("OPI_ORD_DATA_DIR")?),
                journal_dir: PathBuf::from(required("OPI_JOURNAL_DIR")?),
                bitcoin_rpc_url: required("BITCOIN_RPC_URL")?,
                bitcoin_auth,
                chunk,
            },
            brc20_enabled,
        })
    }

    /// The `bitcoin` network matching the configured chain.
    pub fn network(&self) -> bitcoin::Network {
        network_of(self.source.chain)
    }
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
    fn config_from_env_parses_modules_and_defaults() {
        let config = load(&base()).unwrap();
        assert_eq!(config.source.chunk, 1000);
        assert!(config.brc20_enabled);
        assert_eq!(config.source.chain, Chain::Signet);
        assert_eq!(config.network(), bitcoin::Network::Signet);
        assert_eq!(config.source.ord_data_dir, PathBuf::from("/data/ord"));
        assert!(matches!(
            config.source.bitcoin_auth,
            UpstreamAuth::UserPass(ref user, ref pass) if user == "user" && pass == "pass"
        ));

        let mut vars = base();
        vars.insert("OPI_MODULES", "brc20, brc20");
        vars.insert("OPI_CHUNK", "250");
        let config = load(&vars).unwrap();
        assert!(config.brc20_enabled);
        assert_eq!(config.source.chunk, 250);

        vars.insert("OPI_MODULES", "brc20,runes");
        let err = load(&vars).unwrap_err();
        assert!(err.to_string().contains("runes"), "{err}");
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
