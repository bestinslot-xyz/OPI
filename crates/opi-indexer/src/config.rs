//! Process configuration read from `OPI_*` and `BITCOIN_RPC_*` variables.

use {
    anyhow::{Context, Result, anyhow, bail},
    opi_bitmap::BitmapConfig,
    opi_brc20::config::Brc20IndexerConfig,
    opi_runes::RunesConfig,
    opi_sns::SnsConfig,
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
                runes: modules.contains(&ModuleKind::Runes),
            },
            modules,
        })
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

/// Every enabled module's configuration, read and checked before the source
/// starts.
pub struct ModuleConfigs {
    pub brc20: Option<Brc20IndexerConfig>,
    pub bitmap: Option<BitmapConfig>,
    pub sns: Option<SnsConfig>,
    pub runes: Option<RunesConfig>,
}

impl ModuleConfigs {
    pub fn load(config: &Config) -> Result<ModuleConfigs> {
        ModuleConfigs::load_with(config, &|key| std::env::var(key).ok(), || {
            Brc20IndexerConfig::default()
        })
    }

    /// Reads the bitmap, SNS and runes variables through `get` and builds brc20's
    /// configuration with `brc20`. Each enabled module must name its
    /// database; an unset name would fall back to the `postgres` database.
    fn load_with(
        config: &Config,
        get: &dyn Fn(&str) -> Option<String>,
        brc20: impl FnOnce() -> Brc20IndexerConfig,
    ) -> Result<ModuleConfigs> {
        let require = |key: &str| -> Result<()> {
            match get(key).filter(|value| !value.is_empty()) {
                Some(_) => Ok(()),
                None => bail!("{key} is not set; an enabled module needs its database name"),
            }
        };
        let mut brc20 = Some(brc20);
        let mut configs = ModuleConfigs {
            brc20: None,
            bitmap: None,
            sns: None,
            runes: None,
        };
        for module in &config.modules {
            match module {
                ModuleKind::Brc20 => {
                    require("DB_DATABASE")?;
                    let brc20 = brc20.take().expect("parse_modules drops repeats")();
                    if brc20.light_client_mode {
                        bail!("the brc20 module runs in full mode; set OPERATION_MODE=full");
                    }
                    config.check_network(&brc20.network_type_string)?;
                    configs.brc20 = Some(brc20);
                }
                ModuleKind::Bitmap => {
                    require("BITMAP_DB_DATABASE")?;
                    let bitmap = BitmapConfig::from_lookup(get).context("bitmap config")?;
                    config.check_network(&bitmap.network_type)?;
                    configs.bitmap = Some(bitmap);
                }
                ModuleKind::Sns => {
                    require("SNS_DB_DATABASE")?;
                    let sns = SnsConfig::from_lookup(get).context("sns config")?;
                    config.check_network(&sns.network_type)?;
                    configs.sns = Some(sns);
                }
                ModuleKind::Runes => {
                    require("RUNES_DB_DATABASE")?;
                    let runes = RunesConfig::from_lookup(get).context("runes config")?;
                    config.check_network(&runes.network_type)?;
                    configs.runes = Some(runes);
                }
            }
        }
        Ok(configs)
    }
}

/// A module `OPI_MODULES` can enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Brc20,
    Bitmap,
    Sns,
    Runes,
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
            "runes" => ModuleKind::Runes,
            other => {
                bail!(
                    "unknown module `{other}` in OPI_MODULES (supported: brc20, bitmap, sns, runes)"
                )
            }
        };
        if !modules.contains(&module) {
            modules.push(module);
        }
    }
    if modules.is_empty() {
        bail!("OPI_MODULES names no module (supported: brc20, bitmap, sns, runes)");
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

        vars.insert("OPI_MODULES", "runes,sns");
        assert_eq!(
            load(&vars).unwrap().modules,
            vec![ModuleKind::Runes, ModuleKind::Sns]
        );

        vars.insert("OPI_MODULES", "sns");
        assert_eq!(load(&vars).unwrap().modules, vec![ModuleKind::Sns]);

        vars.insert("OPI_MODULES", " , ");
        assert!(load(&vars).is_err());
        vars.insert("OPI_MODULES", "");
        assert!(load(&vars).is_err());
    }

    #[test]
    fn rune_indexing_follows_the_module_list() {
        let cases = [
            (None, false),
            (Some("brc20"), false),
            (Some("bitmap"), false),
            (Some("sns"), false),
            (Some("brc20,bitmap,sns"), false),
            (Some("runes"), true),
            (Some("brc20,runes"), true),
            (Some("sns, runes ,bitmap"), true),
            (Some("brc20,bitmap,sns,runes"), true),
        ];
        for (modules, runes) in cases {
            let mut vars = base();
            if let Some(modules) = modules {
                vars.insert("OPI_MODULES", modules);
            }
            assert_eq!(load(&vars).unwrap().source.runes, runes, "{modules:?}");
        }
    }

    #[test]
    fn unknown_module_is_rejected() {
        let mut vars = base();
        vars.insert("OPI_MODULES", "brc20,ordinals");
        let err = load(&vars).unwrap_err();
        assert!(err.to_string().contains("ordinals"), "{err}");
    }

    #[test]
    fn runes_is_a_module() {
        let mut vars = base();
        vars.insert("OPI_MODULES", "brc20,runes");
        assert_eq!(
            load(&vars).unwrap().modules,
            vec![ModuleKind::Brc20, ModuleKind::Runes]
        );
    }

    #[test]
    fn runes_requires_its_database_name() {
        let signet = ("NETWORK_TYPE", "signet");
        let err = load_modules("runes", &[signet], brc20_signet())
            .err()
            .unwrap();
        assert!(err.to_string().contains("RUNES_DB_DATABASE"), "{err}");
        let err = load_modules(
            "runes",
            &[signet, ("RUNES_DB_DATABASE", "")],
            brc20_signet(),
        )
        .err()
        .unwrap();
        assert!(err.to_string().contains("RUNES_DB_DATABASE"), "{err}");

        let configs = load_modules(
            "runes",
            &[signet, ("RUNES_DB_DATABASE", "opi_runes")],
            brc20_signet(),
        )
        .unwrap();
        assert_eq!(configs.runes.unwrap().db_database, "opi_runes");
        assert!(configs.brc20.is_none());

        // Disabled: the missing name is not an error.
        assert!(load_modules("brc20", &[("DB_DATABASE", "b")], brc20_signet()).is_ok());
    }

    #[test]
    fn runes_config_is_none_when_not_enabled() {
        let configs = load_modules(
            "brc20",
            &[
                ("DB_DATABASE", "b"),
                ("NETWORK_TYPE", "signet"),
                ("RUNES_DB_DATABASE", "opi_runes"),
            ],
            brc20_signet(),
        )
        .unwrap();
        assert!(configs.runes.is_none());
        assert!(configs.brc20.is_some());
    }

    #[test]
    fn runes_network_must_match_the_chain() {
        let err = load_modules(
            "runes",
            &[("NETWORK_TYPE", "mainnet"), ("RUNES_DB_DATABASE", "r")],
            brc20_signet(),
        )
        .err()
        .unwrap();
        assert!(err.to_string().contains("mainnet"), "{err}");
    }

    #[test]
    fn module_env_prefixes_are_independent() {
        let vars = HashMap::from([
            ("BITMAP_DB_DATABASE", "bitmap_db"),
            ("SNS_DB_DATABASE", "sns_db"),
        ]);
        let get = |key: &str| vars.get(key).map(|value| value.to_string());
        let bitmap = BitmapConfig::from_lookup(&get).unwrap();
        let sns = SnsConfig::from_lookup(&get).unwrap();
        assert_eq!(bitmap.db_database, "bitmap_db");
        assert_eq!(sns.db_database, "sns_db");
    }

    /// A brc20 configuration on signet, as `Brc20IndexerConfig::default`
    /// builds it from a signet environment.
    fn brc20_signet() -> Brc20IndexerConfig {
        Brc20IndexerConfig {
            network_type: bitcoin::Network::Signet,
            network_type_string: "signet".into(),
            ..Brc20IndexerConfig::default()
        }
    }

    fn load_modules(
        modules: &'static str,
        env: &[(&'static str, &'static str)],
        brc20: Brc20IndexerConfig,
    ) -> Result<ModuleConfigs> {
        let mut vars = base();
        vars.insert("OPI_MODULES", modules);
        let config = load(&vars)?;
        let env: HashMap<&str, &str> = env.iter().copied().collect();
        ModuleConfigs::load_with(
            &config,
            &|key| env.get(key).map(|value| value.to_string()),
            || brc20,
        )
    }

    #[test]
    fn enabled_modules_need_their_database_name() {
        let signet = ("NETWORK_TYPE", "signet");
        for (module, key) in [
            ("bitmap", "BITMAP_DB_DATABASE"),
            ("sns", "SNS_DB_DATABASE"),
            ("runes", "RUNES_DB_DATABASE"),
            ("brc20", "DB_DATABASE"),
        ] {
            let err = load_modules(module, &[signet], brc20_signet())
                .err()
                .unwrap();
            assert!(err.to_string().contains(key), "{module}: {err}");
            let err = load_modules(module, &[signet, (key, "")], brc20_signet())
                .err()
                .unwrap();
            assert!(err.to_string().contains(key), "{module}: {err}");
            assert!(load_modules(module, &[signet, (key, "opi_db")], brc20_signet()).is_ok());
        }
    }

    #[test]
    fn disabled_modules_need_no_database_name() {
        let configs =
            load_modules("brc20", &[("DB_DATABASE", "brc20_db")], brc20_signet()).unwrap();
        assert!(configs.brc20.is_some());
        assert!(configs.bitmap.is_none() && configs.sns.is_none());

        let configs = load_modules(
            "bitmap",
            &[("NETWORK_TYPE", "signet"), ("BITMAP_DB_DATABASE", "b")],
            brc20_signet(),
        )
        .unwrap();
        assert!(configs.bitmap.is_some());
        assert!(configs.brc20.is_none() && configs.sns.is_none());
    }

    #[test]
    fn brc20_light_mode_is_rejected() {
        let brc20 = Brc20IndexerConfig {
            light_client_mode: true,
            ..brc20_signet()
        };
        let err = load_modules("brc20", &[("DB_DATABASE", "brc20_db")], brc20)
            .err()
            .unwrap();
        assert!(err.to_string().contains("OPERATION_MODE=full"), "{err}");
    }

    #[test]
    fn brc20_network_must_match_the_chain() {
        let brc20 = Brc20IndexerConfig {
            network_type: bitcoin::Network::Bitcoin,
            network_type_string: "mainnet".into(),
            ..Brc20IndexerConfig::default()
        };
        let err = load_modules("brc20", &[("DB_DATABASE", "brc20_db")], brc20)
            .err()
            .unwrap();
        assert!(err.to_string().contains("mainnet"), "{err}");
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
