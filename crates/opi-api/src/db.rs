//! A module's database, queried the way node-pg queries it.
//!
//! node-pg sends a query with parameters as an unnamed prepared statement
//! whose parameters have no declared type, each value as text, so Postgres
//! types `$n` from its context; it reads every result in text format. A
//! query string with each `$n` replaced by an untyped string literal (or
//! `NULL`) gets the same typing, and the simple query protocol answers in
//! text format, which [`pg_json`](crate::pg_json) decodes as node-pg's
//! type parsers do.

use {
    crate::{
        config::DbConfig,
        js::{JsObject, JsValue, json_stringify},
        pg_json::decode_text,
    },
    anyhow::{Result, anyhow, bail},
    futures::TryStreamExt,
    jiff::tz::TimeZone,
    sqlx::{
        AssertSqlSafe, Column, Either, PgPool, Row, TypeInfo, ValueRef,
        postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgSslMode, PgValueFormat},
    },
    std::{sync::Arc, time::Duration},
};

#[cfg(test)]
mod tests;

type FakeHandler = dyn Fn(&str, &[JsValue]) -> Result<Vec<JsObject>> + Send + Sync;

/// A module's database: a Postgres pool, or a test double.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

enum Inner {
    Pg { pool: PgPool, local: TimeZone },
    Fake(Box<FakeHandler>),
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
            // node-pg leaves extra_float_digits at the server's setting
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

    /// Queries `pool`; timestamps without a zone read in the process's
    /// local time zone (TZ), as node does.
    pub fn from_pool(pool: PgPool) -> Db {
        Db {
            inner: Arc::new(Inner::Pg {
                pool,
                local: TimeZone::system(),
            }),
        }
    }

    /// A test double: `handler(sql, params)` answers the rows or an error.
    pub fn fake(
        handler: impl Fn(&str, &[JsValue]) -> Result<Vec<JsObject>> + Send + Sync + 'static,
    ) -> Db {
        Db {
            inner: Arc::new(Inner::Fake(Box::new(handler))),
        }
    }

    /// `db_pool.query(sql, params)` and its `rows`: one object per row,
    /// keyed by column name (a repeated name keeps the last value).
    pub async fn query(&self, sql: &str, params: &[JsValue]) -> Result<Vec<JsObject>> {
        let (pool, local) = match &*self.inner {
            Inner::Fake(handler) => return handler(sql, params),
            Inner::Pg { pool, local } => (pool, local),
        };
        let prepared: Vec<Option<String>> = params.iter().map(prepare_value).collect();
        let text = bind_params(sql, &prepared)?;
        let mut results = sqlx::raw_sql(AssertSqlSafe(text)).fetch_many(pool);
        let mut rows = Vec::new();
        let mut statements = 0;
        while let Some(item) = results.try_next().await? {
            match item {
                Either::Left(_) => statements += 1,
                Either::Right(row) => rows.push(row_object(&row, local)?),
            }
        }
        // node-pg answers several result sets as an array, which has no
        // `rows`; Postgres refuses several statements with parameters.
        if statements > 1 {
            bail!("the query holds {statements} statements; one is expected");
        }
        Ok(rows)
    }
}

fn row_object(row: &PgRow, local: &TimeZone) -> Result<JsObject> {
    let mut obj = JsObject::new();
    for (i, column) in row.columns().iter().enumerate() {
        let type_info = column.type_info();
        let oid = type_info
            .oid()
            .ok_or_else(|| {
                anyhow!(
                    "column {} has no type OID ({})",
                    column.name(),
                    type_info.name()
                )
            })?
            .0;
        let raw = row.try_get_raw(i)?;
        let text = if raw.is_null() {
            None
        } else {
            if raw.format() != PgValueFormat::Text {
                bail!("column {} arrived in binary format", column.name());
            }
            Some(raw.as_str().map_err(|e| anyhow!(e))?)
        };
        let value =
            decode_text(oid, text, local).map_err(|e| anyhow!("column {}: {e}", column.name()))?;
        obj.set(column.name(), value);
    }
    Ok(obj)
}

/// node-pg's prepareValue: the text node-pg sends for a parameter, or
/// `None` for SQL NULL.
pub fn prepare_value(value: &JsValue) -> Option<String> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Array(items) => Some(array_string(items)),
        JsValue::Object(_) => json_stringify(value),
        primitive => Some(
            primitive
                .to_js_string()
                .expect("a primitive converts to a string"),
        ),
    }
}

/// node-pg's arrayString: a Postgres array literal.
fn array_string(items: &[JsValue]) -> String {
    let elements: Vec<String> = items
        .iter()
        .map(|item| match item {
            JsValue::Undefined | JsValue::Null => "NULL".to_string(),
            JsValue::Array(inner) => array_string(inner),
            other => {
                let text = prepare_value(other).unwrap_or_default();
                format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect();
    format!("{{{}}}", elements.join(","))
}

/// A string literal Postgres reads like an untyped text parameter, quoted
/// as libpq's PQescapeLiteral quotes it.
fn literal(value: &str) -> Result<String> {
    if value.contains('\0') {
        bail!("invalid byte sequence for encoding \"UTF8\": 0x00");
    }
    let quoted = value.replace('\'', "''");
    Ok(if value.contains('\\') {
        format!(" E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}

/// Replaces each `$n` placeholder with the literal of `params[n - 1]`,
/// skipping quoted strings, quoted identifiers, dollar-quoted strings and
/// comments. As Postgres does, it refuses a parameter count other than the
/// highest placeholder number.
pub fn bind_params(sql: &str, params: &[Option<String>]) -> Result<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut highest = 0;
    let mut i = 0;
    let prev_is_ident = |i: usize| i > 0 && is_ident_char(chars[i - 1]);
    let copy = |out: &mut String, from: usize, to: usize| out.extend(&chars[from..to]);
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let start = i;
        match c {
            '\'' => {
                let escapes =
                    start > 0 && matches!(chars[start - 1], 'E' | 'e') && !prev_is_ident(start - 1);
                i += 1;
                while i < chars.len() {
                    match chars[i] {
                        '\\' if escapes => i += 2,
                        '\'' if chars.get(i + 1) == Some(&'\'') => i += 2,
                        '\'' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                copy(&mut out, start, i.min(chars.len()));
            }
            '"' => {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '"' {
                        if chars.get(i + 1) == Some(&'"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                copy(&mut out, start, i);
            }
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                copy(&mut out, start, i);
            }
            '/' if next == Some('*') => {
                let mut depth = 0;
                while i < chars.len() {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                copy(&mut out, start, i.min(chars.len()));
            }
            '$' if !prev_is_ident(start) && next.is_some_and(|n| n.is_ascii_digit()) => {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let number: String = chars[start + 1..i].iter().collect();
                let n: usize = number
                    .parse()
                    .map_err(|_| anyhow!("bad placeholder ${number}"))?;
                highest = highest.max(n);
                match params.get(n.wrapping_sub(1)) {
                    Some(Some(value)) => out.push_str(&literal(value)?),
                    Some(None) => out.push_str("NULL"),
                    None => {}
                }
            }
            '$' if !prev_is_ident(start) => {
                // a dollar quote: $$ or $tag$ with tag = [A-Za-z_][A-Za-z0-9_]*
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let tag_ok = chars.get(j) == Some(&'$')
                    && chars.get(i + 1).is_none_or(|c| !c.is_ascii_digit());
                if tag_ok {
                    let tag: String = chars[i..=j].iter().collect();
                    let body_start = j + 1;
                    let rest: String = chars[body_start..].iter().collect();
                    let end = rest.find(&tag).map_or(chars.len(), |p| {
                        body_start + rest[..p].chars().count() + tag.chars().count()
                    });
                    copy(&mut out, start, end);
                    i = end;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    if highest != params.len() {
        bail!(
            "bind message supplies {} parameters, but prepared statement \"\" requires {highest}",
            params.len()
        );
    }
    Ok(out)
}
