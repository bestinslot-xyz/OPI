//! A module's database.
//!
//! Routes query the pool with sqlx's checked macros (`query!`,
//! `query_as!`, `query_scalar!`), whose offline data lives in
//! `crates/opi-api/.sqlx`. Each parameter is the Rust type Postgres infers
//! for it, the type node-pg's untyped text parameters resolve to; the
//! [`params`](crate::params) helpers parse a JS value into that type with
//! Postgres's input rules. Each result column maps to its node-pg JSON form
//! through [`pg_json`](crate::pg_json).
//!
//! sqlx sets the session `TimeZone` to UTC when it connects, where node-pg
//! keeps the server's default. A timestamptz still reaches the client as
//! the same instant: binary values carry no zone, and a timestamptz cast to
//! text carries its offset, which node-pg's date parser applies. SQL whose
//! result depends on the session time zone (`now()::date`, `timestamptz::date`,
//! `to_char` on a timestamptz, `AT TIME ZONE` without a zone name, ...) would
//! answer differently, so no route may use it.

use {
    crate::{config::DbConfig, config::Module, js::JsValue, params::IntRules, pg_json::ToJs},
    anyhow::{Result, anyhow},
    sqlx::{
        PgConnection, PgPool,
        postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
    },
    std::{sync::Arc, time::Duration},
    tokio::sync::OnceCell,
};

#[cfg(test)]
mod tests;

type FakeHeights = dyn Fn(Module) -> Result<JsValue> + Send + Sync;

/// Sets up a new pooled connection's session to plan as node-pg's do.
///
/// node-pg sends every query as an unnamed statement, which Postgres plans
/// at each execution with that execution's parameter values. sqlx caches a
/// named statement per connection, which Postgres may switch to a generic
/// plan from its sixth execution on, and a query without a full ORDER BY
/// (a GROUP BY, say) then lists its rows in another order. Forcing custom
/// plans keeps node-pg's planning for every statement.
pub async fn start_session(connection: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query_scalar!(
        r#"select set_config('plan_cache_mode', 'force_custom_plan', false) as "plan_cache_mode!""#
    )
    .fetch_one(connection)
    .await?;
    Ok(())
}

/// A module's database: a Postgres pool, or a test double that answers
/// only [`Db::latest_block_height`].
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

enum Inner {
    Pg {
        pool: PgPool,
        /// The integer input rules of the server, read once.
        int_rules: OnceCell<IntRules>,
    },
    Fake(Box<FakeHeights>),
}

impl Db {
    /// A pool for `config` that connects on first use, as node-pg's does:
    /// at most `max` connections, and a query fails when no connection is
    /// available within the connect timeout.
    pub fn connect_lazy(config: &DbConfig) -> Result<Db> {
        let mut options = PgConnectOptions::new()
            .host(&config.host)
            .username(&config.user)
            .database(&config.database)
            // ssl: true is TLS without certificate checks, as server.js
            // runs node with NODE_TLS_REJECT_UNAUTHORIZED=0
            .ssl_mode(if config.ssl {
                PgSslMode::Require
            } else {
                PgSslMode::Disable
            })
            // node-pg leaves extra_float_digits at the server's setting,
            // which shapes the float text of a `::text` cast
            .extra_float_digits(None);
        if let Some(port) = config.pg_port()? {
            options = options.port(port);
        }
        // node-pg treats an empty password as unset (PGPASSWORD, .pgpass)
        if let Some(password) = config.password.as_deref().filter(|p| !p.is_empty()) {
            options = options.password(password);
        }
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections()?)
            .acquire_timeout(Duration::from_millis(config.connection_timeout_ms))
            .after_connect(|connection, _| Box::pin(start_session(connection)))
            .connect_lazy_with(options);
        Ok(Db::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Db {
        Db {
            inner: Arc::new(Inner::Pg {
                pool,
                int_rules: OnceCell::new(),
            }),
        }
    }

    /// The integer input rules of the server behind the pool. The server
    /// reports its `server_version` (as `14.15`, say) to every connection
    /// as it starts, and sqlx parses it into a version number, so no query
    /// runs. The first call reads it from a pooled connection and later
    /// calls reuse it; a failed read is retried on the next call. A test
    /// double answers the Postgres 14 rules.
    pub async fn int_rules(&self) -> Result<IntRules> {
        let (pool, int_rules) = match &*self.inner {
            Inner::Fake(_) => return Ok(IntRules::Pg14),
            Inner::Pg { pool, int_rules } => (pool, int_rules),
        };
        let rules = int_rules
            .get_or_try_init(|| async {
                let connection = pool.acquire().await?;
                let version = connection
                    .server_version_num()
                    .ok_or_else(|| anyhow!("the server reported no server_version_num"))?;
                anyhow::Ok(IntRules::for_server_version(version))
            })
            .await?;
        Ok(*rules)
    }

    /// A test double: `heights(module)` answers
    /// [`latest_block_height`](Self::latest_block_height); every other
    /// query fails.
    pub fn fake(heights: impl Fn(Module) -> Result<JsValue> + Send + Sync + 'static) -> Db {
        Db {
            inner: Arc::new(Inner::Fake(Box::new(heights))),
        }
    }

    /// The pool route queries run on. A test double has none, so a route
    /// that queries it fails with the 500 answer.
    pub fn pool(&self) -> Result<&PgPool> {
        match &*self.inner {
            Inner::Pg { pool, .. } => Ok(pool),
            Inner::Fake(_) => Err(anyhow!("a test database runs no queries")),
        }
    }

    /// `(await query('SELECT max(block_height) as max_block_height FROM
    /// <module>_block_hashes;')).rows[0].max_block_height`: a number, or
    /// null for an empty table.
    pub async fn latest_block_height(&self, module: Module) -> Result<JsValue> {
        let pool = match &*self.inner {
            Inner::Fake(heights) => return heights(module),
            Inner::Pg { pool, .. } => pool,
        };
        let height: Option<i32> = match module {
            Module::Brc20 => {
                sqlx::query_scalar!(
                    "SELECT max(block_height) as max_block_height FROM brc20_block_hashes;"
                )
                .fetch_one(pool)
                .await?
            }
            Module::Bitmap => {
                sqlx::query_scalar!(
                    "SELECT max(block_height) as max_block_height FROM bitmap_block_hashes;"
                )
                .fetch_one(pool)
                .await?
            }
            Module::Sns => {
                sqlx::query_scalar!(
                    "SELECT max(block_height) as max_block_height FROM sns_block_hashes;"
                )
                .fetch_one(pool)
                .await?
            }
            Module::Runes => {
                sqlx::query_scalar!(
                    "SELECT max(block_height) as max_block_height FROM runes_block_hashes;"
                )
                .fetch_one(pool)
                .await?
            }
        };
        Ok(height.to_js())
    }
}
