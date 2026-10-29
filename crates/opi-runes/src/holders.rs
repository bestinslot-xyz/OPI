//! Current balances and holder counts per rune, and the indexes of the
//! API's rune routes.
//!
//! - `runes_current_balances` holds, per rune and pkScript, the sum of the
//!   pkScript's unspent outpoints' balances of the rune, with the
//!   pkScript's address. Only positive sums have a row.
//! - `runes_holders` holds, per rune, `holders`: the pkScripts with a row in
//!   `runes_current_balances`, so the distinct owners with a positive
//!   balance over unspent outpoints. Only positive counts have a row.
//!
//! Both tables change by deltas, in the transaction of the write that
//! changes the outpoints: a block adds the balances of the outpoints it
//! creates and subtracts those of the outpoints it spends, and a rollback
//! subtracts the unspent outpoints it removes and adds back those it
//! unspends. One statement upserts each changed (rune, pkScript) balance
//! and moves the rune's count by one for each balance that turns positive
//! or reaches zero, so a block costs in proportion to its own balance
//! changes. Only the start fill counts from scratch. Nothing here feeds an
//! event, hash or report.
//!
//! The tables and indexes are created at start when missing, and a new
//! table pair is filled from the unspent outpoints in the transaction that
//! creates it. The same statements are in db_init.sql.

use sqlx::{PgPool, Postgres, Transaction};

/// The tables, also in db_init.sql.
pub const TABLES_SQL: &str = "CREATE TABLE IF NOT EXISTS public.runes_current_balances (
	rune_id text NOT NULL,
	pkscript text NOT NULL,
	wallet_addr text NULL,
	balance numeric(40) NOT NULL,
	CONSTRAINT runes_current_balances_pk PRIMARY KEY (pkscript, rune_id)
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
/// - `wallet_addr` serves a wallet's balances by address; the primary key,
///   `(pkscript, rune_id)`, serves them by pkScript.
/// - `rune_name text_pattern_ops` serves the name prefix search.
/// - `(rune_id, id)`, `(wallet_addr, id)` and `(pkscript, id)` on
///   `runes_events` serve the rune and wallet event feeds, newest first.
pub const INDEXES: [(&str, &str); 6] = [
    (
        "runes_current_balances_rune_id_balance_idx",
        "CREATE INDEX IF NOT EXISTS runes_current_balances_rune_id_balance_idx ON public.runes_current_balances USING btree (rune_id, balance, pkscript)",
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
    // on the table lock. The builds share one connection with a larger
    // maintenance_work_mem, which a sort of every event needs.
    let mut missing_indexes = Vec::new();
    for (name, sql) in INDEXES {
        if !index_exists(pool, name).await? {
            missing_indexes.push((name, sql));
        }
    }
    if !missing_indexes.is_empty() {
        let mut conn = pool.acquire().await?;
        sqlx::query("SET maintenance_work_mem = '1GB'")
            .execute(&mut *conn)
            .await?;
        let mut announced = false;
        for (name, sql) in missing_indexes {
            if name.starts_with("runes_events_") && !announced {
                tracing::info!(
                    "building runes event indexes, this can take minutes (once, on the first start of this version)"
                );
                announced = true;
            }
            tracing::info!("Creating index {name}");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query("RESET maintenance_work_mem")
            .execute(&mut *conn)
            .await?;
    }
    for name in SUPERSEDED {
        if index_exists(pool, name).await? {
            tracing::info!("Dropping index {name}");
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP INDEX CONCURRENTLY IF EXISTS public.{name}"
            )))
            .execute(pool)
            .await?;
        }
    }
    Ok(missing)
}

/// The statement that applies the balance changes a `changed` CTE lists as
/// `(rune_id, pkscript, wallet_addr, delta)`. It nets them per rune and
/// pkScript, upserts each balance, and moves each rune's count by +1 for a
/// balance that turns positive and -1 for one that reaches zero. The
/// upsert answers each new balance, and the old one is the new less the
/// delta (0 for an inserted row), so no statement looks balances up.
/// It answers the pairs and runes left at zero or below, which the caller
/// deletes: `('b', rune_id, pkscript, balance)` and
/// `('h', rune_id, '', holders)`.
macro_rules! apply_deltas {
    ($changed:literal) => {
        concat!(
            "WITH changed AS (",
            $changed,
            "), net AS ( \
                 SELECT rune_id, pkscript, max(wallet_addr) AS wallet_addr, sum(delta) AS delta \
                 FROM changed GROUP BY rune_id, pkscript HAVING sum(delta) <> 0 \
             ), balances AS ( \
                 INSERT INTO runes_current_balances AS c (rune_id, pkscript, wallet_addr, balance) \
                 SELECT rune_id, pkscript, wallet_addr, delta FROM net \
                 ON CONFLICT (pkscript, rune_id) DO UPDATE SET balance = c.balance + excluded.balance \
                 RETURNING c.rune_id, c.pkscript, c.balance \
             ), moves AS ( \
                 SELECT b.rune_id, \
                        sum(CASE WHEN b.balance - n.delta <= 0 AND b.balance > 0 THEN 1 \
                                 WHEN b.balance - n.delta > 0 AND b.balance <= 0 THEN -1 \
                                 ELSE 0 END)::int8 AS moved \
                 FROM balances b JOIN net n ON n.rune_id = b.rune_id AND n.pkscript = b.pkscript \
                 GROUP BY b.rune_id \
             ), counts AS ( \
                 INSERT INTO runes_holders AS h (rune_id, holders) \
                 SELECT rune_id, moved FROM moves WHERE moved <> 0 \
                 ON CONFLICT (rune_id) DO UPDATE SET holders = h.holders + excluded.holders \
                 RETURNING h.rune_id, h.holders \
             ) \
             SELECT 'b'::text, rune_id, pkscript, balance::text FROM balances WHERE balance <= 0 \
             UNION ALL \
             SELECT 'h'::text, rune_id, ''::text, holders::text FROM counts WHERE holders <= 0"
        )
    };
}

/// Deletes the balances and counts [`apply_deltas`] left at zero or below.
/// One below zero means the tables drifted from the outpoints (a balance
/// subtracted that was never added): it is logged as an error and deleted
/// like the rest. The block goes on, since these tables only serve the API.
async fn drop_emptied(
    tx: &mut Transaction<'_, Postgres>,
    emptied: Vec<(String, String, String, String)>,
) -> Result<(), sqlx::Error> {
    let mut runes = Vec::new();
    let mut pkscripts = Vec::new();
    let mut counted = Vec::new();
    for (kind, rune_id, pkscript, value) in emptied {
        if value.starts_with('-') {
            if kind == "b" {
                tracing::error!(
                    "runes_current_balances drifted: rune {rune_id} pkscript {pkscript} balance {value}; the row is deleted"
                );
            } else {
                tracing::error!(
                    "runes_holders drifted: rune {rune_id} holders {value}; the row is deleted"
                );
            }
        }
        if kind == "b" {
            runes.push(rune_id);
            pkscripts.push(pkscript);
        } else {
            counted.push(rune_id);
        }
    }
    if !runes.is_empty() {
        sqlx::query(
            "DELETE FROM runes_current_balances c \
             USING UNNEST($1::text[], $2::text[]) AS e(rune_id, pkscript) \
             WHERE c.rune_id = e.rune_id AND c.pkscript = e.pkscript AND c.balance <= 0",
        )
        .bind(&runes)
        .bind(&pkscripts)
        .execute(&mut **tx)
        .await?;
    }
    if !counted.is_empty() {
        sqlx::query("DELETE FROM runes_holders WHERE rune_id = ANY($1) AND holders <= 0")
            .bind(&counted)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Applies block `height`'s balance changes: the outpoints it creates add,
/// and those it spends (`spent`) subtract. Runs after the block's outpoints
/// are written and spent.
pub async fn apply_block(
    tx: &mut Transaction<'_, Postgres>,
    height: i32,
    spent: &[String],
) -> Result<(), sqlx::Error> {
    let emptied = sqlx::query_as(apply_deltas!(
        "SELECT u.rune_id, o.pkscript, o.wallet_addr, u.balance AS delta \
         FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
         WHERE o.block_height = $1 \
         UNION ALL \
         SELECT u.rune_id, o.pkscript, o.wallet_addr, -u.balance \
         FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
         WHERE o.outpoint = ANY($2)"
    ))
    .bind(height)
    .bind(spent)
    .fetch_all(&mut **tx)
    .await?;
    drop_emptied(tx, emptied).await
}

/// Reverts the balance changes of every block above `height`, read before
/// the rollback changes the outpoints: the unspent outpoints it removes
/// subtract, and the outpoints it unspends (found through the input events
/// above `height`) add back. An outpoint created and spent above `height`
/// holds no current balance and changes nothing.
pub async fn rollback(tx: &mut Transaction<'_, Postgres>, height: i64) -> Result<(), sqlx::Error> {
    let emptied = sqlx::query_as(apply_deltas!(
        "SELECT u.rune_id, o.pkscript, o.wallet_addr, -u.balance AS delta \
         FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
         WHERE o.block_height > $1 AND NOT o.spent \
         UNION ALL \
         SELECT u.rune_id, o.pkscript, o.wallet_addr, u.balance \
         FROM runes_outpoint_to_balances o, unnest(o.rune_ids, o.balances) AS u(rune_id, balance) \
         WHERE o.outpoint IN (SELECT outpoint FROM runes_events \
                              WHERE block_height > $1 AND event_type = 0) \
         AND o.block_height <= $1 AND o.spent_block_height > $1"
    ))
    .bind(height)
    .fetch_all(&mut **tx)
    .await?;
    drop_emptied(tx, emptied).await
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
