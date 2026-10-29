//! The API's settings, read from the environment. Numeric
//! values are read as `parseInt` reads them, an empty variable takes its
//! default (as JavaScript's `||` does), and only enabled modules need their
//! database.

use {
    crate::js::{js_trim, parse_int_str},
    anyhow::{Result, bail},
    std::{path::Path, time::Duration},
};

#[cfg(test)]
mod tests;

/// A module the API can serve, in the order the app registers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    Brc20,
    Bitmap,
    Sns,
    Runes,
}

pub const MODULES: [Module; 4] = [Module::Brc20, Module::Bitmap, Module::Sns, Module::Runes];

impl Module {
    pub fn name(self) -> &'static str {
        match self {
            Module::Brc20 => "brc20",
            Module::Bitmap => "bitmap",
            Module::Sns => "sns",
            Module::Runes => "runes",
        }
    }

    /// The prefix of the module's database variables; brc20 keeps the
    /// unprefixed `DB_*`.
    pub fn db_prefix(self) -> &'static str {
        match self {
            Module::Brc20 => "DB",
            Module::Bitmap => "BITMAP_DB",
            Module::Sns => "SNS_DB",
            Module::Runes => "RUNES_DB",
        }
    }

    /// The table holding one row per indexed block.
    pub fn block_hashes_table(self) -> &'static str {
        match self {
            Module::Brc20 => "brc20_block_hashes",
            Module::Bitmap => "bitmap_block_hashes",
            Module::Sns => "sns_block_hashes",
            Module::Runes => "runes_block_hashes",
        }
    }
}

/// A module's database, as config.js builds node-pg's pool options.
/// Numbers keep their `parseInt` value, NaN included.
#[derive(Debug, Clone, PartialEq)]
pub struct DbConfig {
    pub prefix: &'static str,
    pub user: String,
    pub host: String,
    pub port: f64,
    pub database: String,
    pub password: Option<String>,
    pub ssl: bool,
    pub max: f64,
    pub connection_timeout_ms: u64,
}

impl DbConfig {
    /// The port to connect to; `None` where node-pg falls back to `PGPORT`
    /// or 5432, which it does for a falsy port (0 or NaN).
    pub fn pg_port(&self) -> Result<Option<u16>> {
        if self.port.is_nan() || self.port == 0.0 {
            return Ok(None);
        }
        if !(1.0..=65535.0).contains(&self.port) {
            bail!(
                "{}_PORT must be a port number, got {}",
                self.prefix,
                self.port
            );
        }
        Ok(Some(self.port as u16))
    }

    /// The pool size. node-pg's pool never fills with a NaN `max` and is
    /// always full with one below 1; both refuse to start here.
    pub fn max_connections(&self) -> Result<u32> {
        if self.max.is_nan() || self.max < 1.0 {
            bail!(
                "DB_MAX_CONNECTIONS must be a positive integer, got {}",
                self.max
            );
        }
        Ok(self.max.min(f64::from(u32::MAX)) as u32)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiSettings {
    pub host: String,
    pub port: f64,
    pub trusted_proxies: f64,
    /// API_SNAPSHOT_TIMEOUT_MS: how long a holders snapshot may stream.
    pub snapshot_timeout_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitSettings {
    pub enabled: bool,
    pub window_ms: f64,
    pub max: f64,
}

/// The push feed's `STREAM_*` variables as `parseInt` reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamVars {
    pub poll_ms: f64,
    pub default_confirmations: f64,
    pub max_confirmations: f64,
    pub max_subscribers: f64,
    pub heartbeat_secs: f64,
    pub buffer: f64,
    pub resume_wait_secs: f64,
}

/// The deepest `STREAM_MAX_CONFIRMATIONS`: the smallest `OPI_PRUNE`, so a
/// pruned module database keeps the block hashes of every height a
/// subscriber may wait on.
pub const STREAM_CONFIRMATIONS_LIMIT: u32 = opi_core::MAX_ROLLBACK_DEPTH;

/// The push feed's settings (`/v1/stream`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSettings {
    /// How often each module's block hashes are read (STREAM_POLL_MS).
    pub poll: Duration,
    /// The confirmations of a request that names none
    /// (STREAM_DEFAULT_CONFIRMATIONS).
    pub default_confirmations: u32,
    /// The most confirmations a request may ask for
    /// (STREAM_MAX_CONFIRMATIONS).
    pub max_confirmations: u32,
    /// The most streams open at once (STREAM_MAX_SUBSCRIBERS).
    pub max_subscribers: usize,
    /// The heartbeat interval, which is also how long a stream may stay
    /// unable to queue a message before it is cut (STREAM_HEARTBEAT_SECS).
    pub heartbeat: Duration,
    /// The chunks a stream queues for its client (STREAM_BUFFER).
    pub buffer: usize,
    /// How long a module's resumed position may wait for its database to
    /// reach it, or for its poller to read the reorg that replaced it,
    /// before it is taken as stale (STREAM_RESUME_WAIT_SECS).
    pub resume_wait: Duration,
}

impl Default for StreamSettings {
    fn default() -> Self {
        StreamSettings {
            poll: Duration::from_millis(1000),
            default_confirmations: 1,
            max_confirmations: 100,
            max_subscribers: 100,
            heartbeat: Duration::from_secs(15),
            buffer: 16,
            resume_wait: Duration::from_secs(300),
        }
    }
}

impl StreamVars {
    /// The settings, or why they cannot run.
    pub fn settings(&self) -> Result<StreamSettings> {
        let whole = |name: &str, value: f64, min: f64, max: f64| -> Result<u64> {
            if value.is_nan() || value < min || value > max {
                bail!("{name} must be an integer from {min} to {max}, got {value}");
            }
            Ok(value as u64)
        };
        let limit = f64::from(STREAM_CONFIRMATIONS_LIMIT);
        let max_confirmations = whole(
            "STREAM_MAX_CONFIRMATIONS",
            self.max_confirmations,
            1.0,
            limit,
        )?;
        let default_confirmations = whole(
            "STREAM_DEFAULT_CONFIRMATIONS",
            self.default_confirmations,
            1.0,
            max_confirmations as f64,
        )?;
        Ok(StreamSettings {
            poll: Duration::from_millis(whole("STREAM_POLL_MS", self.poll_ms, 10.0, 3_600_000.0)?),
            default_confirmations: default_confirmations as u32,
            max_confirmations: max_confirmations as u32,
            max_subscribers: whole(
                "STREAM_MAX_SUBSCRIBERS",
                self.max_subscribers,
                0.0,
                1_000_000.0,
            )? as usize,
            heartbeat: Duration::from_secs(whole(
                "STREAM_HEARTBEAT_SECS",
                self.heartbeat_secs,
                1.0,
                3600.0,
            )?),
            buffer: whole("STREAM_BUFFER", self.buffer, 1.0, 65536.0)? as usize,
            resume_wait: Duration::from_secs(whole(
                "STREAM_RESUME_WAIT_SECS",
                self.resume_wait_secs,
                1.0,
                86400.0,
            )?),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub modules: Vec<Module>,
    pub dbs: Vec<(Module, DbConfig)>,
    pub api: ApiSettings,
    pub rate_limit: RateLimitSettings,
    pub stream: StreamVars,
    /// API_METRICS_ADDR, the metrics listener's address; `None` when it is
    /// unset or empty, which runs no metrics listener.
    pub metrics_addr: Option<String>,
}

impl Config {
    pub fn db(&self, module: Module) -> Option<&DbConfig> {
        self.dbs
            .iter()
            .find(|(m, _)| *m == module)
            .map(|(_, db)| db)
    }

    /// The port to listen on; node's `listen()` throws for anything that is
    /// not an integer from 0 to 65535.
    pub fn listen_port(&self) -> Result<u16> {
        let port = self.api.port;
        if !(0.0..=65535.0).contains(&port) {
            bail!("API_PORT must be a port number from 0 to 65535, got {port}");
        }
        Ok(port as u16)
    }

    /// The hops `trust proxy` = API_TRUSTED_PROXY_CNT trusts: Express
    /// trusts hop `i` when `i < n`, so NaN and negative counts trust none.
    pub fn trusted_hops(&self) -> usize {
        let n = self.api.trusted_proxies;
        if n.is_nan() || n <= 0.0 {
            0
        } else {
            n.ceil().min(usize::MAX as f64) as usize
        }
    }

    /// How long a holders snapshot may stream, at least 1 ms.
    pub fn snapshot_timeout(&self) -> Result<Duration> {
        let ms = self.api.snapshot_timeout_ms;
        if ms.is_nan() || ms < 1.0 {
            bail!("API_SNAPSHOT_TIMEOUT_MS must be a positive number of milliseconds, got {ms}");
        }
        Ok(Duration::from_millis(ms.min(u64::MAX as f64) as u64))
    }

    /// Checks the settings that cannot run as they are, before the server
    /// starts: the listen port, each database's port and pool size, a NaN
    /// rate limit window, the snapshot timeout and the push feed's settings.
    pub fn validate(&self) -> Result<()> {
        self.listen_port()?;
        self.snapshot_timeout()?;
        self.stream.settings()?;
        for (_, db) in &self.dbs {
            db.pg_port()?;
            db.max_connections()?;
        }
        if self.rate_limit.enabled && self.rate_limit.window_ms.is_nan() {
            bail!("RATE_LIMIT_WINDOW_MS must be a number of milliseconds");
        }
        Ok(())
    }
}

/// Parses OPI_MODULES: comma-separated, entries trimmed, empty entries
/// ignored, duplicates dropped. Unset means brc20.
pub fn parse_modules(value: Option<&str>) -> Result<Vec<Module>> {
    let supported = "supported: brc20, bitmap, sns, runes";
    let mut modules = Vec::new();
    for name in value
        .unwrap_or("brc20")
        .split(',')
        .map(js_trim)
        .filter(|name| !name.is_empty())
    {
        let Some(module) = MODULES.into_iter().find(|m| m.name() == name) else {
            bail!("unknown module `{name}` in OPI_MODULES ({supported})");
        };
        if !modules.contains(&module) {
            modules.push(module);
        }
    }
    if modules.is_empty() {
        bail!("OPI_MODULES names no module ({supported})");
    }
    Ok(modules)
}

/// Builds the configuration from environment lookups. An error message is
/// fit for the operator.
pub fn load_config(get: impl Fn(&str) -> Option<String>) -> Result<Config> {
    // `env.X || default`: unset and empty both take the default
    let or = |key: &str, default: &str| {
        get(key)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let modules = parse_modules(get("OPI_MODULES").as_deref())?;
    let max = parse_int_str(&or("DB_MAX_CONNECTIONS", "10"));
    let mut dbs = Vec::new();
    for &module in &modules {
        let prefix = module.db_prefix();
        let var = |name: &str| format!("{prefix}_{name}");
        let Some(database) = get(&var("DATABASE")).filter(|v| !v.is_empty()) else {
            bail!(
                "{} is not set (required because {} is enabled)",
                var("DATABASE"),
                module.name()
            );
        };
        dbs.push((
            module,
            DbConfig {
                prefix,
                user: or(&var("USER"), "postgres"),
                host: or(&var("HOST"), "localhost"),
                port: parse_int_str(&or(&var("PORT"), "5432")),
                database,
                password: get(&var("PASSWD")),
                ssl: get(&var("SSL")).as_deref() == Some("true"),
                max,
                connection_timeout_ms: 5000,
            },
        ));
    }
    Ok(Config {
        modules,
        dbs,
        api: ApiSettings {
            host: or("API_HOST", "127.0.0.1"),
            port: parse_int_str(&or("API_PORT", "8000")),
            trusted_proxies: parse_int_str(&or("API_TRUSTED_PROXY_CNT", "0")),
            snapshot_timeout_ms: parse_int_str(&or("API_SNAPSHOT_TIMEOUT_MS", "600000")),
        },
        rate_limit: RateLimitSettings {
            enabled: or("RATE_LIMIT_ENABLE", "false") == "true",
            window_ms: parse_int_str(&or("RATE_LIMIT_WINDOW_MS", &(15 * 60 * 1000).to_string())),
            max: parse_int_str(&or("RATE_LIMIT_MAX", "100")),
        },
        stream: StreamVars {
            poll_ms: parse_int_str(&or("STREAM_POLL_MS", "1000")),
            default_confirmations: parse_int_str(&or("STREAM_DEFAULT_CONFIRMATIONS", "1")),
            max_confirmations: parse_int_str(&or("STREAM_MAX_CONFIRMATIONS", "100")),
            max_subscribers: parse_int_str(&or("STREAM_MAX_SUBSCRIBERS", "100")),
            heartbeat_secs: parse_int_str(&or("STREAM_HEARTBEAT_SECS", "15")),
            buffer: parse_int_str(&or("STREAM_BUFFER", "16")),
            resume_wait_secs: parse_int_str(&or("STREAM_RESUME_WAIT_SECS", "300")),
        },
        metrics_addr: get("API_METRICS_ADDR").filter(|addr| !addr.is_empty()),
    })
}

/// The variables to add from the env file: the file OPI_ENV_FILE names, or
/// the `.env` opi-indexer reads (the current directory's or an
/// ancestor's). Variables `is_set` reports win; a missing or unreadable file
/// adds nothing.
pub fn env_file_vars(
    opi_env_file: Option<&str>,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let iter = match opi_env_file.filter(|f| !f.is_empty()) {
        Some(file) => dotenvy::from_path_iter(Path::new(file)),
        None => dotenvy::dotenv_iter(),
    };
    let Ok(iter) = iter else {
        return Vec::new();
    };
    iter.filter_map(|item| item.ok())
        .filter(|(key, _)| !is_set(key))
        .collect()
}

/// Loads the env file into the process environment. Call it before any
/// other thread starts.
pub fn load_env() {
    let opi_env_file = std::env::var("OPI_ENV_FILE").ok();
    for (key, value) in env_file_vars(opi_env_file.as_deref(), |key| {
        std::env::var_os(key).is_some()
    }) {
        // SAFETY: called from main before the runtime or any thread starts.
        unsafe { std::env::set_var(key, value) };
    }
}
