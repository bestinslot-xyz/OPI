//! Current balances and holder counts per rune, and the indexes of the
//! API's rune routes.
//!
//! - `runes_current_balances` holds, per rune and pkScript, the sum of the
//!   pkScript's unspent outpoints' balances of the rune, with the
//!   pkScript's address. Only positive sums have a row.
//! - `runes_holders` holds, per rune, `holders`: the pkScripts with a row in
//!   `runes_current_balances`, so the distinct owners with a positive
//!   balance over unspent outpoints.
//!
//! A block's write applies the balances of the outpoints it creates and
//! spends to `runes_current_balances` and recounts the holders of each rune
//! it touches, in the block's transaction. A rollback recomputes the
//! balances of every pkScript whose outpoints it removes or unspends from
//! the outpoints left, and recounts their runes. A recount reads the rune's
//! rows through the primary key, so a block costs one index-only count per
//! rune it touches. Nothing here feeds an event, hash or report.
//!
//! The tables and indexes are created at start when missing, and a new
//! table pair is filled from the unspent outpoints in the transaction that
//! creates it. The same statements are in db_init.sql.

use std::collections::BTreeSet;

use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// The tables, also in db_init.sql.
pub const TABLES_SQL: &str = "CREATE TABLE IF NOT EXISTS public.runes_current_balances (
	rune_id text NOT NULL,
	pkscript text NOT NULL,
	wallet_addr text NULL,
	balance numeric(40) NOT NULL,
	CONSTRAINT runes_current_balances_pk PRIMARY KEY (rune_id, pkscript)
);
CREATE TABLE IF NOT EXISTS public.runes_holders (
	rune_id text NOT NULL,
	holders int8 NOT NULL DEFAULT 0,
	CONSTRAINT runes_holders_pk PRIMARY KEY (rune_id)
);
";

/// The API's indexes by name, each created at start when missing, and in
/// db_init.sql.
///
/// - `(rune_id, balance, pkscript)` serves a rune's holders by balance,
///   walked backwards from a cursor.
/// - `pkscript` and `wallet_addr` serve a wallet's balances (and a
///   rollback's recompute, by pkScript).
/// - `rune_name text_pattern_ops` serves the name prefix search.
/// - `(rune_id, id)`, `(wallet_addr, id)` and `(pkscript, id)` on
///   `runes_events` serve the rune and wallet event feeds, newest first.
pub const INDEXES: [(&str, &str); 7] = [
    (
        "runes_current_balances_rune_id_balance_idx",
        "CREATE INDEX IF NOT EXISTS runes_current_balances_rune_id_balance_idx ON public.runes_current_balances USING btree (rune_id, balance, pkscript)",
    ),
    (
        "runes_current_balances_pkscript_idx",
        "CREATE INDEX IF NOT EXISTS runes_current_balances_pkscript_idx ON public.runes_current_balances USING btree (pkscript)",
    ),
    (
        "runes_current_balances_wallet_addr_idx",
        "CREATE INDEX IF NOT EXISTS runes_current_balances_wallet_addr_idx ON public.runes_current_balances USING btree (wallet_addr)",
    ),
    (
        "runes_id_to_entry_rune_name_pattern_idx",
        "CREATE INDEX IF NOT EXISTS runes_id_to_entry_rune_name_pattern_idx ON public.runes_id_to_entry USING btree (rune_name text_pattern_ops)",
    ),
    (
        "runes_events_rune_id_id_idx",
        "CREATE INDEX IF NOT EXISTS runes_events_rune_id_id_idx ON public.runes_events USING btree (rune_id, id)",
    ),
    (
        "runes_events_wallet_addr_id_idx",
        "CREATE INDEX IF NOT EXISTS runes_events_wallet_addr_id_idx ON public.runes_events USING btree (wallet_addr, id)",
    ),
    (
        "runes_events_pkscript_id_idx",
        "CREATE INDEX IF NOT EXISTS runes_events_pkscript_id_idx ON public.runes_events USING btree (pkscript, id)",
    ),
];

/// The single-column event indexes the `(column, id)` ones extend, dropped
/// at start once those exist.
pub const SUPERSEDED: [&str; 3] = [
    "runes_events_rune_id_idx",
    "runes_events_wallet_addr_idx",
    "runes_events_pkscript_idx",
];

/// The balances of the unspent outpoints of `filter`'s rows, per rune and
/// pkScript.
macro_rules! unspent_sums {
    ($filter:literal) => {
        concat!(
            "INSERT INTO runes_current_balances (rune_id, pkscript, wallet_addr, balance) \
             SELECT u.rune_id, o.pkscript, max(o.wallet_addr), sum(u.balance) \
             FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
             WHERE NOT o.spent",
            $filter,
            " GROUP BY u.rune_id, o.pkscript HAVING sum(u.balance) > 0"
        )
    };
}

async fn index_exists(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?
            .is_some(),
    )
}

/// Creates the tables when either is missing, filled from the unspent
/// outpoints in the transaction that creates them, then each missing index,
/// and drops the superseded event indexes. Returns whether it filled the
/// tables.
pub async fn create_if_missing(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let missing: bool = sqlx::query_scalar(
        "SELECT to_regclass('public.runes_current_balances') IS NULL \
         OR to_regclass('public.runes_holders') IS NULL",
    )
    .fetch_one(pool)
    .await?;
    if missing {
        tracing::info!("Filling runes_current_balances and runes_holders");
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(
            "DROP TABLE IF EXISTS public.runes_current_balances; \
             DROP TABLE IF EXISTS public.runes_holders;",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::raw_sql(TABLES_SQL).execute(&mut *tx).await?;
        sqlx::query(unspent_sums!("")).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO runes_holders (rune_id, holders) \
             SELECT rune_id, count(*) FROM runes_current_balances GROUP BY rune_id",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    // The lookup comes first, so a start that finds the index never waits
    // on the table lock.
    for (name, sql) in INDEXES {
        if !index_exists(pool, name).await? {
            tracing::info!("Creating index {name}");
            sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
        }
    }
    for name in SUPERSEDED {
        if index_exists(pool, name).await? {
            tracing::info!("Dropping index {name}");
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP INDEX IF EXISTS public.{name}"
            )))
            .execute(pool)
            .await?;
        }
    }
    Ok(missing)
}

/// Recounts the holders of `runes` from `runes_current_balances`; a rune
/// without an entry loses its row.
async fn recount(conn: &mut PgConnection, runes: BTreeSet<String>) -> Result<(), sqlx::Error> {
    if runes.is_empty() {
        return Ok(());
    }
    let runes: Vec<String> = runes.into_iter().collect();
    sqlx::query(
        "INSERT INTO runes_holders (rune_id, holders) \
         SELECT r.rune_id, (SELECT count(*) FROM runes_current_balances c \
                            WHERE c.rune_id = r.rune_id) \
         FROM UNNEST($1::text[]) AS r(rune_id) \
         ON CONFLICT (rune_id) DO UPDATE SET holders = excluded.holders",
    )
    .bind(&runes)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM runes_holders h WHERE h.rune_id = ANY($1) \
         AND NOT EXISTS (SELECT 1 FROM runes_id_to_entry e WHERE e.rune_id = h.rune_id)",
    )
    .bind(&runes)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Applies block `height`'s outpoints to the current balances and recounts
/// the runes they hold: those it creates add, and those it spends
/// (`spent`) subtract. Runs after the block's outpoints are written and
/// spent.
pub async fn apply_block(
    tx: &mut Transaction<'_, Postgres>,
    height: i32,
    spent: &[String],
) -> Result<(), sqlx::Error> {
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "WITH changed AS ( \
             SELECT u.rune_id, o.pkscript, o.wallet_addr, u.balance AS delta \
             FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
             WHERE o.block_height = $1 \
             UNION ALL \
             SELECT u.rune_id, o.pkscript, o.wallet_addr, -u.balance \
             FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
             WHERE o.outpoint = ANY($2) \
         ) \
         INSERT INTO runes_current_balances AS c (rune_id, pkscript, wallet_addr, balance) \
         SELECT rune_id, pkscript, max(wallet_addr), sum(delta) FROM changed \
         GROUP BY rune_id, pkscript HAVING sum(delta) <> 0 \
         ON CONFLICT (rune_id, pkscript) DO UPDATE SET balance = c.balance + excluded.balance \
         RETURNING c.rune_id, c.pkscript, c.balance <= 0",
    )
    .bind(height)
    .bind(spent)
    .fetch_all(&mut **tx)
    .await?;
    let (emptied_runes, emptied_pkscripts): (Vec<String>, Vec<String>) = rows
        .iter()
        .filter(|row| row.2)
        .map(|row| (row.0.clone(), row.1.clone()))
        .unzip();
    if !emptied_runes.is_empty() {
        sqlx::query(
            "DELETE FROM runes_current_balances c \
             USING UNNEST($1::text[], $2::text[]) AS e(rune_id, pkscript) \
             WHERE c.rune_id = e.rune_id AND c.pkscript = e.pkscript AND c.balance <= 0",
        )
        .bind(&emptied_runes)
        .bind(&emptied_pkscripts)
        .execute(&mut **tx)
        .await?;
    }
    recount(tx, rows.into_iter().map(|row| row.0).collect()).await
}

/// The pkScripts whose outpoints a rollback to `height` removes or
/// unspends, read before it runs.
pub async fn rollback_pkscripts(
    tx: &mut Transaction<'_, Postgres>,
    height: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT pkscript FROM runes_outpoint_to_balances WHERE block_height > $1 \
         UNION \
         SELECT pkscript FROM runes_outpoint_to_balances \
         WHERE outpoint IN (SELECT outpoint FROM runes_events \
                            WHERE block_height > $1 AND event_type = 0) \
         AND spent_block_height > $1",
    )
    .bind(height)
    .fetch_all(&mut **tx)
    .await
}

/// Recomputes the current balances of `pkscripts` from the outpoints a
/// rollback left, and recounts the runes they held before or hold now.
pub async fn rollback(
    tx: &mut Transaction<'_, Postgres>,
    pkscripts: &[String],
) -> Result<(), sqlx::Error> {
    if pkscripts.is_empty() {
        return Ok(());
    }
    let mut runes: BTreeSet<String> = sqlx::query_scalar(
        "DELETE FROM runes_current_balances WHERE pkscript = ANY($1) RETURNING rune_id",
    )
    .bind(pkscripts)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let restored: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(concat!(
        unspent_sums!(" AND o.pkscript = ANY($1)"),
        " RETURNING rune_id"
    )))
    .bind(pkscripts)
    .fetch_all(&mut **tx)
    .await?;
    runes.extend(restored);
    recount(tx, runes).await
}

/// Empties both tables, for a rollback that drops every block.
pub async fn clear(tx: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::raw_sql("DELETE FROM runes_current_balances; DELETE FROM runes_holders;")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_init_creates_the_tables_and_indexes() {
        let init = include_str!("db_init.sql");
        assert!(init.contains(TABLES_SQL));
        for (name, sql) in INDEXES {
            assert!(init.contains(&format!("{sql};")), "{name}");
            assert!(sql.contains(name), "{name}");
        }
        for name in SUPERSEDED {
            assert!(!init.contains(&format!("{name} ")), "{name}");
        }
    }
}
