use anyhow::Context;
use bitcoin::network::Network;
use brc20_prog::Brc20ProgConfig;
use std::{collections::HashMap, path::PathBuf};

lazy_static::lazy_static! {
    pub static ref FIRST_INSCRIPTION_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 767_430);
        map.insert(Network::Testnet, 2_413_343);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 112_402);
        map
    };

    pub static ref FIRST_BRC20_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 779_832);
        map.insert(Network::Testnet, 2_413_343);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 112_402);
        map
    };

    /// Height at which self-minting is activated
    pub static ref SELF_MINT_ACTIVATION_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 837_090);
        map.insert(Network::Testnet, 2_413_343);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 0);
        map
    };

    /// During phase 1, only 6 byte tickers can get deposited into programmable module.
    pub static ref FIRST_BRC20_PROG_PHASE_1_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 912_690);
        map.insert(Network::Testnet, 0);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 230_000);
        map
    };

    /// During phase 2, all tickers can get deposited into programmable module.
    pub static ref FIRST_BRC20_PROG_PHASE_2_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 934_888);
        map.insert(Network::Testnet, 0);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 230_000);
        map
    };

    /// Height at which brc20_prog events include the tx_id field (Prague upgrade).
    pub static ref BRC20_PROG_PRAGUE_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 923_369);
        map.insert(Network::Testnet, 0);
        map.insert(Network::Testnet4, 0);
        map.insert(Network::Regtest, 0);
        map.insert(Network::Signet, 275_000);
        map
    };

    /// Height for BRC20 swap module refunds activation
    /// Proposal: https://github.com/brc20-devs/brc20-proposals/blob/main/bp08-module-swap-refund/proposal.md
    pub static ref BRC20_SWAP_REFUND_ACTIVATION_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 932_888); // Per proposal
        map.insert(Network::Testnet, i32::MAX); // Unset
        map.insert(Network::Testnet4, i32::MAX); // Unset
        map.insert(Network::Regtest, i32::MAX); // Unset
        map.insert(Network::Signet, 283_888); // Per proposal
        map
    };

    /// BRC20 swap refund addresses per network
    /// Proposal: https://github.com/brc20-devs/brc20-proposals/blob/main/bp08-module-swap-refund/proposal.md
    pub static ref BRC20_SWAP_REFUND_ADDRESS: HashMap<Network, & 'static str> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, "bc1qrj03km5h9ag24cpynyn3l5tvny9cyd0x0le42u");
        map.insert(Network::Testnet, "");
        map.insert(Network::Testnet4, "");
        map.insert(Network::Regtest, "");
        map.insert(Network::Signet, "tb1qkrewl9zclku2qngth52eezdyrwmjpcspttdypa");
        map
    };

    /// BRC20 swap module OP_RETURN pkScripts per network
    /// Proposal: https://github.com/brc20-devs/brc20-proposals/blob/main/bp08-module-swap-refund/proposal.md
    pub static ref BRC20_SWAP_MODULE_PKSCRIPT: HashMap<Network, & 'static str> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, "6a208cbc2ac9896cac98d304aa42f43b98208ce8ae31c25e48d84ee852834a1a8066");
        map.insert(Network::Testnet, "");
        map.insert(Network::Testnet4, "");
        map.insert(Network::Regtest, "");
        map.insert(Network::Signet, "6a2031152ea2364a6dbaab9013be8d656a34256d7e89f0c30714bbb04a9d85e63e5b");
        map
    };
}

pub const DB_HOST_KEY: &str = "DB_HOST";
pub const DB_HOST_DEFAULT: &str = "localhost";

pub const DB_PORT_KEY: &str = "DB_PORT";
pub const DB_PORT_DEFAULT: &str = "5432";

pub const DB_USER_KEY: &str = "DB_USER";
pub const DB_USER_DEFAULT: &str = "postgres";

pub const DB_PASSWORD_KEY: &str = "DB_PASSWD";
pub const DB_PASSWORD_DEFAULT: &str = "";

pub const DB_DATABASE_KEY: &str = "DB_DATABASE";
pub const DB_DATABASE_DEFAULT: &str = "postgres";

pub const DB_SSL_KEY: &str = "DB_SSL";
pub const DB_SSL_DEFAULT: &str = "false";

pub const REPORT_TO_INDEXER_KEY: &str = "REPORT_TO_INDEXER";
pub const REPORT_TO_INDEXER_DEFAULT: &str = "true";

pub const REPORT_ALL_BLOCKS_KEY: &str = "REPORT_ALL_BLOCKS";
pub const REPORT_ALL_BLOCKS_DEFAULT: &str = "false";

pub const REPORT_URL_KEY: &str = "REPORT_URL";
pub const REPORT_URL_DEFAULT: &str = "https://api.opi.network/report_block";

pub const REPORT_RETRIES_KEY: &str = "REPORT_RETRIES";
pub const REPORT_RETRIES_DEFAULT: &str = "10";

pub const REPORT_NAME_KEY: &str = "REPORT_NAME";
pub const REPORT_NAME_DEFAULT: &str = "opi_brc20_indexer";

pub const NETWORK_TYPE_KEY: &str = "NETWORK_TYPE";
pub const NETWORK_TYPE_DEFAULT: &str = "mainnet";

pub const HEIGHT_LIMIT_KEY: &str = "HEIGHT_LIMIT";
pub const HEIGHT_LIMIT_DEFAULT: i32 = i32::MAX;

pub const PROTOCOL_KEY: &str = "p";
pub const PROTOCOL_BRC20: &str = "brc-20";
pub const PROTOCOL_BRC20_PROG: &str = "brc20-prog";
pub const PROTOCOL_BRC20_MODULE: &str = "brc20-module";

pub const BRC20_MODULE_BRC20PROG: &str = "BRC20PROG";

pub const BITCOIN_RPC_CACHE_ENABLED_KEY: &str = "BITCOIN_RPC_CACHE_ENABLED";
pub const BITCOIN_RPC_CACHE_ENABLED_DEFAULT: &str = "false";

pub const BITCOIN_RPC_PROXY_SERVER_ENABLED: &str = "BITCOIN_RPC_PROXY_SERVER_ENABLED";
pub const BITCOIN_RPC_PROXY_SERVER_ENABLED_DEFAULT: &str = "false";

pub const BITCOIN_RPC_PROXY_SERVER_ADDR_KEY: &str = "BITCOIN_RPC_PROXY_SERVER_ADDR";
pub const BITCOIN_RPC_PROXY_SERVER_ADDR_DEFAULT: &str = "127.0.0.1:18547";

pub const BITCOIN_RPC_URL_KEY: &str = "BITCOIN_RPC_URL";
pub const BITCOIN_RPC_URL_DEFAULT: &str = "http://localhost:38332";

pub const SAVE_LOGS_KEY: &str = "SAVE_LOGS";
pub const SAVE_LOGS_DEFAULT: &str = "true";

// BRC20 specific keys
pub const LIMIT_PER_MINT_KEY: &str = "lim";
pub const MAX_SUPPLY_KEY: &str = "max";
pub const DECIMALS_KEY: &str = "dec";
pub const AMOUNT_KEY: &str = "amt";
pub const OPERATION_KEY: &str = "op";
pub const MODULE_KEY: &str = "module";
pub const TICKER_KEY: &str = "tick";
pub const SELF_MINT_KEY: &str = "self_mint";
pub const SALT_KEY: &str = "salt";
pub const HASH_KEY: &str = "hash";

// BRC20 prog specific keys
pub const DATA_KEY: &str = "d";
pub const BASE64_DATA_KEY: &str = "b";
pub const CONTRACT_ADDRESS_KEY: &str = "c";
pub const INSCRIPTION_ID_KEY: &str = "i";

pub const OPERATION_DEPLOY: &str = "deploy";
pub const OPERATION_PREDEPLOY: &str = "predeploy";
pub const OPERATION_WITHDRAW: &str = "withdraw";
pub const OPERATION_MINT: &str = "mint";
pub const OPERATION_TRANSFER: &str = "transfer";

pub const OPERATION_BRC20_PROG_DEPLOY: &str = "deploy";
pub const OPERATION_BRC20_PROG_DEPLOY_SHORT: &str = "d";

pub const OPERATION_BRC20_PROG_CALL: &str = "call";
pub const OPERATION_BRC20_PROG_CALL_SHORT: &str = "c";

pub const OPERATION_BRC20_PROG_TRANSACT: &str = "transact";
pub const OPERATION_BRC20_PROG_TRANSACT_SHORT: &str = "t";

pub const BRC20_PROG_OP_RETURN_PKSCRIPT: &str = "6a09425243323050524f47";
pub const OP_RETURN: &str = "6a";
pub const NO_TX_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub const NO_WALLET: &str = "";

pub const MAX_DECIMALS: u8 = 18;
pub const MAX_AMOUNT: u128 = (2u128.pow(64) - 1) * 10u128.pow(18);

pub const PREDEPLOY_BLOCK_HEIGHT_DELAY: i32 = 3;
pub const PREDEPLOY_BLOCK_HEIGHT_ACCEPTANCE_DELAY: i32 = 10;

pub const BRC20_PROG_MINE_BATCH_SIZE: i32 = 50000;

pub const EVENT_SEPARATOR: &str = "|";

pub const OPERATION_MODE_KEY: &str = "OPERATION_MODE";
pub const OPERATION_MODE_FULL: &str = "full";
pub const OPERATION_MODE_LIGHT: &str = "light";

// Versions used for database migrations and version checks
// These should be updated when the database schema changes
pub const DB_VERSION: i32 = 7;
pub const EVENT_HASH_VERSION: i32 = 3;
pub const BRC20_PROG_VERSION_REQUIREMENT: &str = "~0.16.0";
pub const INDEXER_VERSION: &str = concat!("opi-brc20-rs-node v", env!("CARGO_PKG_VERSION"));
pub const LIGHT_CLIENT_VERSION: &str =
    concat!("opi-brc20-rs-node-light v", env!("CARGO_PKG_VERSION"));

pub const OPI_API_URL: &str = match option_env!("OPI_API_URL") {
    Some(url) => url,
    None => "https://api.opi.network",
};

fn get_bitcoin_network_type(network_type: &str) -> Network {
    match network_type {
        "mainnet" => Network::Bitcoin,
        "testnet" => Network::Testnet,
        "testnet4" => Network::Testnet4,
        "regtest" => Network::Regtest,
        "signet" => Network::Signet,
        _ => panic!("Invalid network type"),
    }
}

pub const BRC20_PROG_DB_PATH_KEY: &str = "BRC20_PROG_DB_PATH";
pub const BRC20_PROG_RPC_SERVER_ENABLED_KEY: &str = "BRC20_PROG_RPC_SERVER_ENABLED";
pub const BITCOIN_RPC_USER_KEY: &str = "BITCOIN_RPC_USER";
pub const BITCOIN_RPC_PASSWD_KEY: &str = "BITCOIN_RPC_PASSWD";
pub const BITCOIN_RPC_COOKIE_KEY: &str = "BITCOIN_RPC_COOKIE";

/// How the Bitcoin RPC proxy authenticates to bitcoind.
#[derive(Clone, PartialEq, Eq)]
pub enum BitcoinRpcAuth {
    UserPass(String, String),
    /// bitcoind's cookie file, read on every request since bitcoind rewrites
    /// it on restart.
    Cookie(PathBuf),
    /// No credentials, for a bitcoind (or light-client host) that needs none.
    None,
}

impl std::fmt::Debug for BitcoinRpcAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BitcoinRpcAuth::UserPass(user, _) => write!(f, "UserPass({user:?}, \"***\")"),
            BitcoinRpcAuth::Cookie(path) => write!(f, "Cookie({path:?})"),
            BitcoinRpcAuth::None => write!(f, "None"),
        }
    }
}

impl BitcoinRpcAuth {
    /// Parses `BITCOIN_RPC_COOKIE`, or `BITCOIN_RPC_USER` with
    /// `BITCOIN_RPC_PASSWD`, by opi-indexer's rules; neither set is `None`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        match (
            get(BITCOIN_RPC_COOKIE_KEY).filter(|value| !value.is_empty()),
            get(BITCOIN_RPC_USER_KEY).filter(|value| !value.is_empty()),
            get(BITCOIN_RPC_PASSWD_KEY),
        ) {
            (Some(cookie), None, _) => Ok(BitcoinRpcAuth::Cookie(PathBuf::from(cookie))),
            (None, Some(user), Some(passwd)) => Ok(BitcoinRpcAuth::UserPass(user, passwd)),
            (None, Some(_), None) => {
                anyhow::bail!("{BITCOIN_RPC_USER_KEY} is set without {BITCOIN_RPC_PASSWD_KEY}")
            }
            (Some(_), Some(_), _) => anyhow::bail!(
                "set either {BITCOIN_RPC_COOKIE_KEY} or {BITCOIN_RPC_USER_KEY}, not both"
            ),
            (None, None, _) => Ok(BitcoinRpcAuth::None),
        }
    }

    /// Parses the process environment (see `from_lookup`).
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// The user and password to present now, reading the cookie file for
    /// cookie auth; `None` presents no credentials.
    pub async fn credentials(&self) -> anyhow::Result<Option<(String, String)>> {
        match self {
            BitcoinRpcAuth::UserPass(user, pass) => Ok(Some((user.clone(), pass.clone()))),
            BitcoinRpcAuth::Cookie(path) => {
                let cookie = tokio::fs::read_to_string(path)
                    .await
                    .with_context(|| format!("reading cookie file {}", path.display()))?;
                let (user, pass) = cookie
                    .trim_end()
                    .split_once(':')
                    .with_context(|| format!("malformed cookie file {}", path.display()))?;
                Ok(Some((user.into(), pass.into())))
            }
            BitcoinRpcAuth::None => Ok(None),
        }
    }
}

/// Chain id brc20-prog uses on mainnet ("BRC20" in hex).
const BRC20_PROG_CHAIN_ID_MAINNET: u64 = 0x4252433230;
/// Chain id brc20-prog uses on every other network ("BRC20s" in hex).
const BRC20_PROG_CHAIN_ID_TESTNETS: u64 = 0x425243323073;

/// Whether the embedded engine serves its public JSON-RPC. On when unset or
/// exactly `true`; any other value, including an empty one, turns it off.
fn parse_rpc_server_enabled(value: Option<&str>) -> bool {
    value.unwrap_or("true") == "true"
}

/// Applies the settings opi-indexer owns to the engine config `base`
/// (normally `Brc20ProgConfig::from_env()`):
///
/// - `db_path` comes from the required `BRC20_PROG_DB_PATH`.
/// - `bitcoin_rpc_network` and `chain_id` follow `network_type` (the `NETWORK_TYPE`
///   value): `mainnet` maps to `bitcoin` and the mainnet chain id, every other
///   network keeps its name and takes the testnets chain id.
/// - `bitcoin_rpc_url` is the Bitcoin RPC proxy when `proxy_addr` is set, with
///   no credentials, since the proxy authenticates to bitcoind itself.
///   Without the proxy it is `bitcoin_rpc_url`, and the engine needs
///   `BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`.
fn apply_engine_settings(
    mut base: Brc20ProgConfig,
    get: impl Fn(&str) -> Option<String>,
    network_type: &str,
    proxy_addr: Option<&str>,
    bitcoin_rpc_url: &str,
) -> anyhow::Result<Brc20ProgConfig> {
    base.db_path = get(BRC20_PROG_DB_PATH_KEY)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{BRC20_PROG_DB_PATH_KEY} is not set"))?;

    if network_type == "mainnet" {
        base.bitcoin_rpc_network = "bitcoin".to_string();
        base.chain_id = BRC20_PROG_CHAIN_ID_MAINNET;
    } else {
        base.bitcoin_rpc_network = network_type.to_string();
        base.chain_id = BRC20_PROG_CHAIN_ID_TESTNETS;
    }

    match proxy_addr {
        Some(addr) => {
            base.bitcoin_rpc_url = format!("http://{addr}");
            base.bitcoin_rpc_user = String::new();
            base.bitcoin_rpc_password = String::new();
        }
        None => {
            base.bitcoin_rpc_url = bitcoin_rpc_url.to_string();
            let user = get(BITCOIN_RPC_USER_KEY).filter(|value| !value.is_empty());
            let password = get(BITCOIN_RPC_PASSWD_KEY).filter(|value| !value.is_empty());
            let (Some(user), Some(password)) = (user, password) else {
                anyhow::bail!(
                    "brc20-prog needs {BITCOIN_RPC_USER_KEY} and {BITCOIN_RPC_PASSWD_KEY} when the Bitcoin RPC proxy is disabled; enable the proxy (BITCOIN_RPC_PROXY_SERVER_ENABLED=true) or set them"
                );
            };
            base.bitcoin_rpc_user = user;
            base.bitcoin_rpc_password = password;
        }
    }

    Ok(base)
}

#[derive(Clone)]
pub struct Brc20IndexerConfig {
    pub light_client_mode: bool,
    pub save_logs: bool,

    pub db_host: String,
    pub db_port: String,
    pub db_user: String,
    pub db_password: String,
    pub db_database: String,
    pub db_ssl: bool,

    pub report_to_indexer: bool,
    pub report_all_blocks: bool,
    pub report_url: String,
    pub report_retries: i32,
    pub report_name: String,

    pub network_type: Network,
    pub network_type_string: String,

    pub first_inscription_height: i32,
    pub first_brc20_height: i32,
    /// Phase 1 adds support for contracts and depositing tickers with 6 byte length.
    pub first_brc20_prog_phase_one_height: i32,
    /// Phase 2 adds support for all tickers.
    pub first_brc20_prog_all_tickers_height: i32,
    /// Height at which tx_id field was added to brc20_prog events.
    pub first_brc20_prog_prague_height: i32,
    /// Self mint activation height
    pub self_mint_activation_height: i32,

    /// BRC20 swap refund activation height
    pub brc20_swap_refund_activation_height: i32,
    /// BRC20 swap refund address
    pub brc20_swap_refund_address: String,
    /// BRC20 swap module pkScript
    pub brc20_swap_module_pkscript: String,

    pub brc20_prog_bitcoin_rpc_proxy_server_enabled: bool,
    pub brc20_prog_bitcoin_rpc_proxy_server_addr: String,
    /// Whether the embedded engine serves its public JSON-RPC (`BRC20_PROG_RPC_SERVER_ENABLED`).
    pub brc20_prog_rpc_server_enabled: bool,

    pub bitcoin_rpc_cache_enabled: bool,
    pub bitcoin_rpc_url: String,
    /// How the Bitcoin RPC proxy authenticates to bitcoind.
    pub bitcoin_rpc_auth: BitcoinRpcAuth,

    pub height_limit: i32,
}

impl Brc20IndexerConfig {
    /// Builds the embedded engine's config from brc20-prog's own environment
    /// variables plus the settings opi-indexer owns (see `apply_engine_settings`).
    /// Errors when `BRC20_PROG_DB_PATH` is not set.
    pub fn brc20_prog_config(&self) -> anyhow::Result<Brc20ProgConfig> {
        let proxy_addr = self
            .brc20_prog_bitcoin_rpc_proxy_server_enabled
            .then_some(self.brc20_prog_bitcoin_rpc_proxy_server_addr.as_str());
        apply_engine_settings(
            Brc20ProgConfig::from_env(),
            |key| std::env::var(key).ok(),
            &self.network_type_string,
            proxy_addr,
            &self.bitcoin_rpc_url,
        )
    }
}

impl Default for Brc20IndexerConfig {
    fn default() -> Self {
        let network_type_string =
            &std::env::var(NETWORK_TYPE_KEY).unwrap_or_else(|_| NETWORK_TYPE_DEFAULT.to_string());
        let network_type = get_bitcoin_network_type(network_type_string);

        Brc20IndexerConfig {
            db_host: std::env::var(DB_HOST_KEY).unwrap_or_else(|_| DB_HOST_DEFAULT.to_string()),
            db_port: std::env::var(DB_PORT_KEY).unwrap_or_else(|_| DB_PORT_DEFAULT.to_string()),
            db_user: std::env::var(DB_USER_KEY).unwrap_or_else(|_| DB_USER_DEFAULT.to_string()),
            db_password: std::env::var(DB_PASSWORD_KEY)
                .unwrap_or_else(|_| DB_PASSWORD_DEFAULT.to_string()),
            db_database: std::env::var(DB_DATABASE_KEY)
                .unwrap_or_else(|_| DB_DATABASE_DEFAULT.to_string()),
            db_ssl: std::env::var(DB_SSL_KEY).unwrap_or_else(|_| DB_SSL_DEFAULT.to_string())
                == "true",

            report_to_indexer: std::env::var(REPORT_TO_INDEXER_KEY)
                .unwrap_or_else(|_| REPORT_TO_INDEXER_DEFAULT.to_string())
                == "true"
                && network_type != Network::Regtest,

            report_all_blocks: std::env::var(REPORT_ALL_BLOCKS_KEY)
                .unwrap_or_else(|_| REPORT_ALL_BLOCKS_DEFAULT.to_string())
                == "true",

            report_url: std::env::var(REPORT_URL_KEY)
                .unwrap_or_else(|_| REPORT_URL_DEFAULT.to_string()),
            report_retries: std::env::var(REPORT_RETRIES_KEY)
                .unwrap_or_else(|_| REPORT_RETRIES_DEFAULT.to_string())
                .parse::<i32>()
                .unwrap(),
            report_name: std::env::var(REPORT_NAME_KEY)
                .unwrap_or_else(|_| REPORT_NAME_DEFAULT.to_string()),

            network_type,
            network_type_string: network_type_string.to_string(),

            first_inscription_height: *FIRST_INSCRIPTION_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            first_brc20_height: *FIRST_BRC20_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            first_brc20_prog_phase_one_height: *FIRST_BRC20_PROG_PHASE_1_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            first_brc20_prog_all_tickers_height: *FIRST_BRC20_PROG_PHASE_2_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            first_brc20_prog_prague_height: *BRC20_PROG_PRAGUE_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            self_mint_activation_height: *SELF_MINT_ACTIVATION_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            brc20_swap_refund_activation_height: *BRC20_SWAP_REFUND_ACTIVATION_HEIGHTS
                .get(&network_type)
                .unwrap_or_else(|| panic!("Invalid network type: {}", network_type)),
            brc20_swap_refund_address: BRC20_SWAP_REFUND_ADDRESS
                .get(&network_type)
                .unwrap_or(&"")
                .to_string(),
            brc20_swap_module_pkscript: BRC20_SWAP_MODULE_PKSCRIPT
                .get(&network_type)
                .unwrap_or(&"")
                .to_string(),

            light_client_mode: std::env::var(OPERATION_MODE_KEY)
                .unwrap_or_else(|_| OPERATION_MODE_FULL.to_string())
                == OPERATION_MODE_LIGHT,
            save_logs: std::env::var(SAVE_LOGS_KEY)
                .unwrap_or_else(|_| SAVE_LOGS_DEFAULT.to_string())
                == "true",

            brc20_prog_bitcoin_rpc_proxy_server_enabled: std::env::var(
                BITCOIN_RPC_PROXY_SERVER_ENABLED,
            )
            .unwrap_or_else(|_| BITCOIN_RPC_PROXY_SERVER_ENABLED_DEFAULT.to_string())
                == "true",
            brc20_prog_bitcoin_rpc_proxy_server_addr: std::env::var(
                BITCOIN_RPC_PROXY_SERVER_ADDR_KEY,
            )
            .unwrap_or_else(|_| BITCOIN_RPC_PROXY_SERVER_ADDR_DEFAULT.to_string()),
            brc20_prog_rpc_server_enabled: parse_rpc_server_enabled(
                std::env::var(BRC20_PROG_RPC_SERVER_ENABLED_KEY)
                    .ok()
                    .as_deref(),
            ),

            bitcoin_rpc_url: std::env::var(BITCOIN_RPC_URL_KEY)
                .unwrap_or_else(|_| BITCOIN_RPC_URL_DEFAULT.to_string()),
            bitcoin_rpc_cache_enabled: std::env::var(BITCOIN_RPC_CACHE_ENABLED_KEY)
                .unwrap_or_else(|_| BITCOIN_RPC_CACHE_ENABLED_DEFAULT.to_string())
                == "true",
            bitcoin_rpc_auth: BitcoinRpcAuth::from_env().unwrap_or_else(|e| panic!("{e}")),

            height_limit: std::env::var(HEIGHT_LIMIT_KEY)
                .unwrap_or_else(|_| HEIGHT_LIMIT_DEFAULT.to_string())
                .parse::<i32>()
                .unwrap_or(HEIGHT_LIMIT_DEFAULT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn engine(
        pairs: &[(&str, &str)],
        network: &str,
        proxy_addr: Option<&str>,
    ) -> anyhow::Result<Brc20ProgConfig> {
        // A pure base: brc20-prog's own BITCOIN_RPC_PASSWORD and BITCOIN_RPC_NETWORK
        // are overridden by apply_engine_settings.
        let base = Brc20ProgConfig::new(
            "127.0.0.1:18545".to_string(),
            false,
            None,
            None,
            false,
            1_000_000_000,
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            0,
            true,
            String::new(),
            10 * 1024 * 1024,
            100 * 1024 * 1024,
            50,
        );
        apply_engine_settings(base, vars(pairs), network, proxy_addr, "http://btc:8332")
    }

    const DB: (&str, &str) = (BRC20_PROG_DB_PATH_KEY, "/data/prog");
    const USER: (&str, &str) = (BITCOIN_RPC_USER_KEY, "u");
    const PASSWD: (&str, &str) = (BITCOIN_RPC_PASSWD_KEY, "p");

    #[test]
    fn proxy_disabled_requires_user_and_password() {
        for pairs in [
            vec![DB],
            vec![DB, USER],
            vec![DB, PASSWD],
            vec![DB, USER, (BITCOIN_RPC_PASSWD_KEY, "")],
            vec![DB, (BITCOIN_RPC_USER_KEY, ""), PASSWD],
        ] {
            let err = engine(&pairs, "signet", None).unwrap_err().to_string();
            assert!(
                err.contains("BITCOIN_RPC_USER and BITCOIN_RPC_PASSWD"),
                "{err}"
            );
        }
    }

    #[test]
    fn proxy_enabled_does_not_require_credentials() {
        assert!(engine(&[DB], "signet", Some("127.0.0.1:18547")).is_ok());
    }

    #[test]
    fn mainnet_maps_to_bitcoin_and_mainnet_chain_id() {
        let config = engine(&[DB, USER, PASSWD], "mainnet", None).unwrap();
        assert_eq!(config.bitcoin_rpc_network, "bitcoin");
        assert_eq!(config.chain_id, 0x4252433230);
    }

    #[test]
    fn other_networks_keep_name_and_use_testnets_chain_id() {
        for network in ["signet", "testnet", "testnet4", "regtest"] {
            let config = engine(&[DB, USER, PASSWD], network, None).unwrap();
            assert_eq!(config.bitcoin_rpc_network, network);
            assert_eq!(config.chain_id, 0x425243323073, "{network}");
        }
    }

    #[test]
    fn proxy_enabled_points_at_proxy_without_credentials() {
        let config = engine(
            &[
                DB,
                (BITCOIN_RPC_USER_KEY, "u"),
                (BITCOIN_RPC_PASSWD_KEY, "p"),
            ],
            "signet",
            Some("127.0.0.1:18547"),
        )
        .unwrap();
        assert_eq!(config.bitcoin_rpc_url, "http://127.0.0.1:18547");
        assert_eq!(config.bitcoin_rpc_user, "");
        assert_eq!(config.bitcoin_rpc_password, "");
    }

    #[test]
    fn proxy_disabled_uses_bitcoin_rpc_url_and_credentials() {
        let config = engine(
            &[
                DB,
                (BITCOIN_RPC_USER_KEY, "u"),
                (BITCOIN_RPC_PASSWD_KEY, "p"),
            ],
            "signet",
            None,
        )
        .unwrap();
        assert_eq!(config.bitcoin_rpc_url, "http://btc:8332");
        assert_eq!(config.bitcoin_rpc_user, "u");
        assert_eq!(config.bitcoin_rpc_password, "p");
    }

    #[test]
    fn db_path_is_taken_from_env() {
        let config = engine(&[DB, USER, PASSWD], "signet", None).unwrap();
        assert_eq!(config.db_path, "/data/prog");
    }

    #[test]
    fn missing_or_empty_db_path_errors() {
        let err = engine(&[USER, PASSWD], "signet", None).unwrap_err();
        assert!(err.to_string().contains("BRC20_PROG_DB_PATH"));
        assert!(
            engine(
                &[(BRC20_PROG_DB_PATH_KEY, ""), USER, PASSWD],
                "signet",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn bitcoin_rpc_auth_follows_opi_indexer_rules() {
        let auth = |pairs: &[(&str, &str)]| BitcoinRpcAuth::from_lookup(vars(pairs));
        const COOKIE: (&str, &str) = (BITCOIN_RPC_COOKIE_KEY, "/btc/.cookie");
        assert_eq!(
            auth(&[COOKIE]).unwrap(),
            BitcoinRpcAuth::Cookie(PathBuf::from("/btc/.cookie"))
        );
        assert_eq!(
            auth(&[COOKIE, PASSWD]).unwrap(),
            BitcoinRpcAuth::Cookie(PathBuf::from("/btc/.cookie"))
        );
        assert_eq!(
            auth(&[USER, PASSWD]).unwrap(),
            BitcoinRpcAuth::UserPass("u".into(), "p".into())
        );
        assert_eq!(
            auth(&[USER, (BITCOIN_RPC_PASSWD_KEY, "")]).unwrap(),
            BitcoinRpcAuth::UserPass("u".into(), String::new())
        );
        assert_eq!(auth(&[]).unwrap(), BitcoinRpcAuth::None);
        assert_eq!(
            auth(&[(BITCOIN_RPC_COOKIE_KEY, ""), (BITCOIN_RPC_USER_KEY, "")]).unwrap(),
            BitcoinRpcAuth::None
        );
        assert_eq!(
            auth(&[USER]).unwrap_err().to_string(),
            "BITCOIN_RPC_USER is set without BITCOIN_RPC_PASSWD"
        );
        assert_eq!(
            auth(&[COOKIE, USER, PASSWD]).unwrap_err().to_string(),
            "set either BITCOIN_RPC_COOKIE or BITCOIN_RPC_USER, not both"
        );
    }

    #[test]
    fn bitcoin_rpc_auth_debug_hides_the_password() {
        let auth = BitcoinRpcAuth::UserPass("u".into(), "secret".into());
        assert!(!format!("{auth:?}").contains("secret"));
    }

    #[test]
    fn rpc_server_enabled_is_on_only_when_unset_or_true() {
        assert!(parse_rpc_server_enabled(None));
        assert!(parse_rpc_server_enabled(Some("true")));
        for off in ["false", "0", "False", "no", ""] {
            assert!(!parse_rpc_server_enabled(Some(off)), "{off:?}");
        }
    }
}
