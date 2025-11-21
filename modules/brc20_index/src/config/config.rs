use bitcoin::network::Network;
use std::collections::HashMap;

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

    pub static ref FIRST_BRC20_PROG_PHASE_2_HEIGHTS: HashMap<Network, i32> = {
        let mut map = HashMap::new();
        map.insert(Network::Bitcoin, 9_999_999); // Unset, waiting for finalization
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

pub const META_DB_URL_KEY: &str = "META_DB_URL";
pub const META_DB_URL_DEFAULT: &str = "http://localhost:11030";

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

pub const BRC20_PROG_ENABLED_KEY: &str = "BRC20_PROG_ENABLED";
pub const BRC20_PROG_ENABLED_DEFAULT: &str = "false";

pub const BRC20_PROG_RPC_URL_KEY: &str = "BRC20_PROG_RPC_URL";
pub const BRC20_PROG_RPC_URL_DEFAULT: &str = "http://localhost:18545";

pub const BRC20_PROG_RPC_USER_KEY: &str = "BRC20_PROG_RPC_USER";
pub const BRC20_PROG_RPC_PASSWORD_KEY: &str = "BRC20_PROG_RPC_PASSWORD";

pub const BRC20_PROG_BALANCE_SERVER_ADDR_KEY: &str = "BRC20_PROG_BALANCE_SERVER_ADDR";
pub const BRC20_PROG_BALANCE_SERVER_ADDR_DEFAULT: &str = "127.0.0.1:18546";

pub const SAVEPOINT_INTERVAL_KEY: &str = "SAVEPOINT_INTERVAL";
pub const SAVEPOINT_INTERVAL_DEFAULT: i32 = 10;

pub const MAX_SAVEPOINTS_KEY: &str = "MAX_SAVEPOINTS";
pub const MAX_SAVEPOINTS_DEFAULT: i32 = 2;

pub const NON_INTERACTIVE: &str = "NON_INTERACTIVE";
pub const NON_INTERACTIVE_DEFAULT: &str = "false";

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

pub const STARTUP_WAIT_SECONDS_KEY: &str = "STARTUP_WAIT_SECONDS";
pub const STARTUP_WAIT_SECONDS_DEFAULT: u64 = 1;

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

pub const OPI_DB_URL_KEY: &str = "OPI_DB_URL";
pub const OPI_DB_URL_DEFAULT: &str = "http://localhost:11030";

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
pub const EVENT_HASH_VERSION: i32 = 2;
pub const BRC20_PROG_VERSION_REQUIREMENT: &str = "~0.15.0";
pub const INDEXER_VERSION: &str = "opi-brc20-rs-node v0.1.0";
pub const LIGHT_CLIENT_VERSION: &str = "opi-brc20-rs-node-light v0.1.0";

pub const OPI_URL: &str = "https://api.opi.network";

pub fn get_startup_wait_secs() -> u64 {
    std::env::var(STARTUP_WAIT_SECONDS_KEY)
        .unwrap_or_else(|_| STARTUP_WAIT_SECONDS_DEFAULT.to_string())
        .parse::<u64>()
        .unwrap_or(STARTUP_WAIT_SECONDS_DEFAULT)
}

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

pub struct Brc20IndexerConfig {
    pub light_client_mode: bool,
    pub save_logs: bool,

    pub db_host: String,
    pub db_port: String,
    pub db_user: String,
    pub db_password: String,
    pub db_database: String,
    pub db_ssl: bool,

    pub opi_db_url: String,

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

    pub brc20_prog_enabled: bool,
    pub brc20_prog_rpc_url: String,
    pub brc20_prog_rpc_user: Option<String>,
    pub brc20_prog_rpc_password: Option<String>,

    pub savepoint_interval: i32,
    pub max_savepoints: i32,

    pub brc20_prog_balance_server_addr: String,

    pub brc20_prog_bitcoin_rpc_proxy_server_enabled: bool,
    pub brc20_prog_bitcoin_rpc_proxy_server_addr: String,

    pub bitcoin_rpc_cache_enabled: bool,
    pub bitcoin_rpc_url: String,

    pub non_interactive: bool,
}

impl Default for Brc20IndexerConfig {
    fn default() -> Self {
        let network_type_string =
            &std::env::var(NETWORK_TYPE_KEY).unwrap_or_else(|_| NETWORK_TYPE_DEFAULT.to_string());
        let network_type = get_bitcoin_network_type(&network_type_string);

        let config = Brc20IndexerConfig {
            db_host: std::env::var(DB_HOST_KEY).unwrap_or_else(|_| DB_HOST_DEFAULT.to_string()),
            db_port: std::env::var(DB_PORT_KEY).unwrap_or_else(|_| DB_PORT_DEFAULT.to_string()),
            db_user: std::env::var(DB_USER_KEY).unwrap_or_else(|_| DB_USER_DEFAULT.to_string()),
            db_password: std::env::var(DB_PASSWORD_KEY)
                .unwrap_or_else(|_| DB_PASSWORD_DEFAULT.to_string()),
            db_database: std::env::var(DB_DATABASE_KEY)
                .unwrap_or_else(|_| DB_DATABASE_DEFAULT.to_string()),
            db_ssl: std::env::var(DB_SSL_KEY).unwrap_or_else(|_| DB_SSL_DEFAULT.to_string())
                == "true",

            opi_db_url: std::env::var(OPI_DB_URL_KEY)
                .unwrap_or_else(|_| OPI_DB_URL_DEFAULT.to_string()),

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

            brc20_prog_enabled: std::env::var(BRC20_PROG_ENABLED_KEY)
                .unwrap_or_else(|_| BRC20_PROG_ENABLED_DEFAULT.to_string())
                == "true",
            brc20_prog_rpc_url: std::env::var(BRC20_PROG_RPC_URL_KEY)
                .unwrap_or_else(|_| BRC20_PROG_RPC_URL_DEFAULT.to_string()),
            brc20_prog_rpc_user: std::env::var(BRC20_PROG_RPC_USER_KEY)
                .ok()
                .filter(|s| !s.is_empty()),
            brc20_prog_rpc_password: std::env::var(BRC20_PROG_RPC_PASSWORD_KEY)
                .ok()
                .filter(|s| !s.is_empty()),

            savepoint_interval: std::env::var(SAVEPOINT_INTERVAL_KEY)
                .unwrap_or_else(|_| SAVEPOINT_INTERVAL_DEFAULT.to_string())
                .parse::<i32>()
                .unwrap_or(SAVEPOINT_INTERVAL_DEFAULT),
            
            max_savepoints: std::env::var(MAX_SAVEPOINTS_KEY)
                .unwrap_or_else(|_| MAX_SAVEPOINTS_DEFAULT.to_string())
                .parse::<i32>()
                .unwrap_or(MAX_SAVEPOINTS_DEFAULT),

            brc20_prog_balance_server_addr: std::env::var(BRC20_PROG_BALANCE_SERVER_ADDR_KEY)
                .unwrap_or_else(|_| BRC20_PROG_BALANCE_SERVER_ADDR_DEFAULT.to_string()),
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

            bitcoin_rpc_url: std::env::var(BITCOIN_RPC_URL_KEY)
                .unwrap_or_else(|_| BITCOIN_RPC_URL_DEFAULT.to_string()),
            bitcoin_rpc_cache_enabled: std::env::var(BITCOIN_RPC_CACHE_ENABLED_KEY)
                .unwrap_or_else(|_| BITCOIN_RPC_CACHE_ENABLED_DEFAULT.to_string())
                == "true",

            non_interactive: std::env::var(NON_INTERACTIVE)
                .unwrap_or_else(|_| NON_INTERACTIVE_DEFAULT.to_string())
                == "true",
        };

        config
    }
}
