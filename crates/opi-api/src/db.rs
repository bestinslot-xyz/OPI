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
    crate::{config::DbConfig, config::Module, js::JsValue, pg_json::ToJs},
    anyhow::{Result, anyhow},
    sqlx::{
        PgPool,
        postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
    },
    std::{sync::Arc, time::Duration},
};

#[cfg(test)]
mod tests;

type FakeHeights = dyn Fn(Module) -> Result<JsValue> + Send + Sync;

/// A module's database: a Postgres pool, or a test double that answers
/// only [`Db::latest_block_height`].
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

enum Inner {
    Pg(PgPool),
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
            .connect_lazy_with(options);
        Ok(Db::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Db {
        Db {
            inner: Arc::new(Inner::Pg(pool)),
        }
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
            Inner::Pg(pool) => Ok(pool),
            Inner::Fake(_) => Err(anyhow!("a test database runs no queries")),
        }
    }

    /// `(await query('SELECT max(block_height) as max_block_height FROM
    /// <module>_block_hashes;')).rows[0].max_block_height`: a number, or
    /// null for an empty table.
    pub async fn latest_block_height(&self, module: Module) -> Result<JsValue> {
        let pool = match &*self.inner {
            Inner::Fake(heights) => return heights(module),
            Inner::Pg(pool) => pool,
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
