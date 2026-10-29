//! The modules on the stream: what each reads for a block, from its own
//! database. A module joins the stream with one [`Provider`] variant and
//! its arms here.

use {
    super::super::{
        bitmap::BitmapRow,
        brc20::events::{self, EVENT_DECIMALS},
        sns::SnsRow,
    },
    crate::{config::Module, db::Db, js::JsValue, reply::ApiError},
};

/// A module the stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    Bitmap,
    Brc20,
    Sns,
}

/// A row of a module's block-hash table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub height: i32,
    pub hash: String,
    /// The block's time in seconds, for the modules that store it.
    pub time: Option<i64>,
}

impl Provider {
    /// The provider of `module`, or why the stream does not carry it.
    pub fn of(module: Module) -> Result<Provider, String> {
        match module {
            Module::Brc20 => Ok(Provider::Brc20),
            Module::Bitmap => Ok(Provider::Bitmap),
            Module::Sns => Ok(Provider::Sns),
            Module::Runes => Err("runes is not on the stream yet".into()),
        }
    }

    pub fn module(self) -> Module {
        match self {
            Provider::Brc20 => Module::Brc20,
            Provider::Bitmap => Module::Bitmap,
            Provider::Sns => Module::Sns,
        }
    }

    pub fn name(self) -> &'static str {
        self.module().name()
    }

    /// The fields a block message carries after `block_time`, each as
    /// `,"name":value`: brc20's amount `decimals`.
    pub fn header_fields(self) -> String {
        match self {
            Provider::Brc20 => format!(",\"decimals\":{EVENT_DECIMALS}"),
            Provider::Bitmap | Provider::Sns => String::new(),
        }
    }

    /// The highest row of the block-hash table, `None` for an empty table.
    pub async fn tip(self, db: &Db) -> Result<Option<(i32, String)>, ApiError> {
        let pool = db.pool()?;
        let row = match self {
            Provider::Brc20 => sqlx::query!(
                "select block_height, block_hash from brc20_block_hashes order by block_height desc limit 1;"
            )
            .fetch_optional(pool)
            .await?
            .map(|row| (row.block_height, row.block_hash)),
            Provider::Bitmap => sqlx::query!(
                "select block_height, block_hash from bitmap_block_hashes order by block_height desc limit 1;"
            )
            .fetch_optional(pool)
            .await?
            .map(|row| (row.block_height, row.block_hash)),
            Provider::Sns => sqlx::query!(
                "select block_height, block_hash from sns_block_hashes order by block_height desc limit 1;"
            )
            .fetch_optional(pool)
            .await?
            .map(|row| (row.block_height, row.block_hash)),
        };
        Ok(row)
    }

    /// The lowest height of the block-hash table (pruning deletes the rows
    /// below its window), `None` for an empty table.
    pub async fn oldest(self, db: &Db) -> Result<Option<i32>, ApiError> {
        let pool = db.pool()?;
        let height = match self {
            Provider::Brc20 => {
                sqlx::query_scalar!(r#"select min(block_height) as "h?" from brc20_block_hashes;"#)
                    .fetch_one(pool)
                    .await?
            }
            Provider::Bitmap => {
                sqlx::query_scalar!(r#"select min(block_height) as "h?" from bitmap_block_hashes;"#)
                    .fetch_one(pool)
                    .await?
            }
            Provider::Sns => {
                sqlx::query_scalar!(r#"select min(block_height) as "h?" from sns_block_hashes;"#)
                    .fetch_one(pool)
                    .await?
            }
        };
        Ok(height)
    }

    /// The rows of the block-hash table from `from` to `to`, ascending, in
    /// one statement, so they come from one snapshot.
    pub async fn blocks(self, db: &Db, from: i32, to: i32) -> Result<Vec<BlockRow>, ApiError> {
        let pool = db.pool()?;
        let rows = match self {
            Provider::Brc20 => sqlx::query!(
                "select block_height, block_hash, block_time from brc20_block_hashes
                    where block_height between $1 and $2 order by block_height;",
                from,
                to,
            )
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| BlockRow {
                height: row.block_height,
                hash: row.block_hash,
                time: row.block_time,
            })
            .collect(),
            Provider::Bitmap => sqlx::query!(
                "select block_height, block_hash from bitmap_block_hashes
                    where block_height between $1 and $2 order by block_height;",
                from,
                to,
            )
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| BlockRow {
                height: row.block_height,
                hash: row.block_hash,
                time: None,
            })
            .collect(),
            Provider::Sns => sqlx::query!(
                "select block_height, block_hash from sns_block_hashes
                    where block_height between $1 and $2 order by block_height;",
                from,
                to,
            )
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| BlockRow {
                height: row.block_height,
                hash: row.block_hash,
                time: None,
            })
            .collect(),
        };
        Ok(rows)
    }

    /// The lowest and the highest id of block `height`'s events, `None` for
    /// a block without events.
    pub async fn event_ids(self, db: &Db, height: i32) -> Result<Option<(i64, i64)>, ApiError> {
        let pool = db.pool()?;
        let range = match self {
            Provider::Brc20 => return events::block_event_ids(db, height).await,
            Provider::Bitmap => {
                let row = sqlx::query!(
                    r#"select min(id) as "lo?", max(id) as "hi?" from bitmaps where block_height = $1;"#,
                    height,
                )
                .fetch_one(pool)
                .await?;
                row.lo.zip(row.hi)
            }
            Provider::Sns => {
                let row = sqlx::query!(
                    r#"select min(id) as "lo?", max(id) as "hi?" from sns_names where block_height = $1;"#,
                    height,
                )
                .fetch_one(pool)
                .await?;
                row.lo.zip(row.hi)
            }
        };
        Ok(range)
    }

    /// Up to `limit` of block `height`'s events with an id above `after`
    /// and at most `last`, oldest first, each with its id: brc20's events in
    /// the event feeds' form, the bitmaps and the names the block
    /// registered in the lookup routes' form.
    pub async fn events(
        self,
        db: &Db,
        height: i32,
        after: i64,
        last: i64,
        limit: i64,
    ) -> Result<Vec<(i64, JsValue)>, ApiError> {
        let pool = db.pool()?;
        let events = match self {
            Provider::Brc20 => return events::block_events(db, height, after, last, limit).await,
            Provider::Bitmap => sqlx::query!(
                "select id, inscription_id, inscription_number, bitmap_number, block_height
                    from bitmaps
                    where id > $2 and id <= $3 and block_height = $1
                    order by id limit $4;",
                height,
                after,
                last,
                limit,
            )
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| {
                let bitmap = BitmapRow {
                    inscription_id: row.inscription_id,
                    inscription_number: row.inscription_number,
                    bitmap_number: row.bitmap_number,
                    block_height: row.block_height,
                };
                (row.id, JsValue::from(bitmap.into_object()))
            })
            .collect(),
            Provider::Sns => sqlx::query!(
                r#"select id, inscription_id, inscription_number, "name", domain, block_height
                    from sns_names
                    where id > $2 and id <= $3 and block_height = $1
                    order by id limit $4;"#,
                height,
                after,
                last,
                limit,
            )
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| {
                let name = SnsRow {
                    inscription_id: row.inscription_id,
                    inscription_number: row.inscription_number,
                    name: row.name,
                    namespace: row.domain,
                    block_height: row.block_height,
                };
                (row.id, JsValue::from(name.into_object()))
            })
            .collect(),
        };
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_order_by_module_name() {
        let mut providers = [Provider::Sns, Provider::Brc20, Provider::Bitmap];
        providers.sort();
        let names: Vec<_> = providers.iter().map(|p| p.name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
