use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex, PoisonError},
};

use axum::body::Bytes;
use sqlx::{Pool, Postgres};

/// A Bitcoin RPC result waiting to be committed with its block.
#[derive(Debug, Clone)]
pub struct BitcoinRpcResultRecord {
    pub method: String,
    pub response: serde_json::Value,
    pub block_height: i32,
}

/// Results cached since the last commit, keyed by the request without its id.
/// Shared by the indexer's database and the engine's Bitcoin RPC source. The lock is
/// never held across an await, so neither side can block the other.
pub type BitcoinRpcBuffer = Arc<Mutex<HashMap<serde_json::Value, BitcoinRpcResultRecord>>>;

/// A copy of the buffered results, for a commit to write.
pub(crate) fn snapshot_buffer(
    buffer: &BitcoinRpcBuffer,
) -> Vec<(serde_json::Value, BitcoinRpcResultRecord)> {
    buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(request, record)| (request.clone(), record.clone()))
        .collect()
}

/// Drops the results a commit wrote. A result buffered after the snapshot
/// stays for the next commit.
pub(crate) fn remove_committed(buffer: &BitcoinRpcBuffer, requests: &[serde_json::Value]) {
    let mut buffered = buffer.lock().unwrap_or_else(PoisonError::into_inner);
    for request in requests {
        buffered.remove(request);
    }
}

/// The Bitcoin RPC result cache: a buffer of uncommitted results in front of
/// the `brc20_bitcoin_rpc_result_cache` table.
///
/// The indexer's database and the Bitcoin RPC source each hold one, built on their own
/// Postgres pool and sharing one buffer. A pool's connections are driven by the
/// runtime that opened them, so each side reads and writes the table only
/// through its own pool.
#[derive(Debug, Clone)]
pub struct BitcoinRpcCache {
    pool: Pool<Postgres>,
    buffer: BitcoinRpcBuffer,
    enabled: bool,
    first_inscription_height: i32,
}

impl BitcoinRpcCache {
    pub fn new(
        pool: Pool<Postgres>,
        buffer: BitcoinRpcBuffer,
        enabled: bool,
        first_inscription_height: i32,
    ) -> Self {
        BitcoinRpcCache {
            pool,
            buffer,
            enabled,
            first_inscription_height,
        }
    }

    /// The same cache and buffer on another pool.
    pub fn on_pool(&self, pool: Pool<Postgres>) -> Self {
        BitcoinRpcCache {
            pool,
            ..self.clone()
        }
    }

    pub fn buffer(&self) -> &BitcoinRpcBuffer {
        &self.buffer
    }

    pub async fn get_current_block_height(&self) -> Result<i32, Box<dyn Error>> {
        select_current_block_height(&self.pool, self.first_inscription_height).await
    }

    /// The answer in the in-memory buffer: this block's lookups, not yet
    /// committed.
    pub fn get_buffered(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
        if !self.enabled {
            return Ok(None);
        }
        let request = ParsedRequest::parse(request)?;
        let response = self
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&request.without_id)
            .map(|x| x.response.clone());
        response
            .map(|response| request.answer(response))
            .transpose()
    }

    /// The newest answer in the `brc20_bitcoin_rpc_result_cache` table.
    pub async fn get_stored(&self, request: &[u8]) -> Result<Option<Bytes>, Box<dyn Error>> {
        if !self.enabled {
            return Ok(None);
        }
        let request = ParsedRequest::parse(request)?;
        let Some(response) =
            select_cached_response(&self.pool, &request.method, &request.without_id).await?
        else {
            return Ok(None);
        };
        request.answer(response).map(Some)
    }

    /// Whether `brc20_block_hashes` holds `block_hash`, through its
    /// `block_hash` index.
    pub async fn is_block_indexed(&self, block_hash: &str) -> Result<bool, Box<dyn Error>> {
        Ok(
            sqlx::query("SELECT 1 FROM brc20_block_hashes WHERE block_hash = $1 LIMIT 1")
                .bind(block_hash)
                .fetch_optional(&self.pool)
                .await?
                .is_some(),
        )
    }

    /// Buffers the response for the next commit.
    pub async fn cache(&self, request: &[u8], response: &[u8]) -> Result<(), Box<dyn Error>> {
        if !self.enabled {
            return Ok(());
        }

        let Ok(request_json) =
            serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(request))
        else {
            return Err("Failed to parse Bitcoin RPC request as JSON".into());
        };

        let request_json_without_id = request_json
            .as_object()
            .map(|obj| {
                let mut obj_clone = obj.clone();
                obj_clone.remove("id");
                serde_json::Value::Object(obj_clone)
            })
            .unwrap_or(request_json);

        let Some(request_method) = request_json_without_id
            .get("method")
            .and_then(|m| m.as_str())
        else {
            return Err("Bitcoin RPC request does not contain a method".into());
        };

        let Ok(response_json) =
            serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(response))
        else {
            return Err("Failed to parse Bitcoin RPC response as JSON".into());
        };

        let response_json_without_id = response_json
            .as_object()
            .map(|obj| {
                let mut obj_clone = obj.clone();
                obj_clone.remove("id");
                serde_json::Value::Object(obj_clone)
            })
            .unwrap_or(response_json);

        let block_height = self.get_current_block_height().await? + 1;
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                request_json_without_id.clone(),
                BitcoinRpcResultRecord {
                    block_height,
                    method: request_method.to_string(),
                    response: response_json_without_id,
                },
            );

        Ok(())
    }
}

/// A cache request split into its key (the request without its `id`), the
/// method and the `id` its answer carries.
struct ParsedRequest {
    without_id: serde_json::Value,
    method: String,
    id: Option<serde_json::Value>,
}

impl ParsedRequest {
    fn parse(request: &[u8]) -> Result<Self, Box<dyn Error>> {
        let Ok(request_json) = serde_json::from_slice::<serde_json::Value>(request) else {
            return Err("Failed to parse Bitcoin RPC request as JSON".into());
        };
        let Some(object) = request_json.as_object() else {
            return Err("Bitcoin RPC request does not contain a valid JSON object".into());
        };
        let id = object.get("id").cloned();
        let mut without_id = object.clone();
        without_id.remove("id");
        let without_id = serde_json::Value::Object(without_id);
        let method = without_id
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or("Bitcoin RPC request does not contain a method")?
            .to_string();
        tracing::debug!(
            "Fetching Bitcoin RPC request from cache: {:?} with id {:?}",
            request_json,
            id
        );
        Ok(ParsedRequest {
            without_id,
            method,
            id,
        })
    }

    /// The stored `response` as bytes, carrying the request's `id` if it had one.
    fn answer(&self, mut response: serde_json::Value) -> Result<Bytes, Box<dyn Error>> {
        if let (Some(id), Some(object)) = (&self.id, response.as_object_mut()) {
            object.insert("id".to_string(), id.clone());
        }
        Ok(Bytes::from(serde_json::to_vec(&response)?))
    }
}

/// The highest indexed block, or `first_inscription_height - 1` before the
/// first one. A database error is an error.
pub(crate) async fn select_current_block_height(
    pool: &Pool<Postgres>,
    first_inscription_height: i32,
) -> Result<i32, Box<dyn Error>> {
    let row = sqlx::query!(
        "SELECT block_height FROM brc20_block_hashes ORDER BY block_height DESC LIMIT 1"
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map_or(first_inscription_height - 1, |row| row.block_height))
}

/// The newest row (highest block height) for the request.
async fn select_cached_response(
    pool: &Pool<Postgres>,
    method: &str,
    request_without_id: &serde_json::Value,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT response FROM brc20_bitcoin_rpc_result_cache WHERE method = $1 AND request = $2 ORDER BY block_height DESC LIMIT 1",
        method,
        request_without_id
    )
    .fetch_optional(pool)
    .await?
    .map(|row| row.response))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(method: &str) -> BitcoinRpcResultRecord {
        BitcoinRpcResultRecord {
            method: method.to_string(),
            response: serde_json::json!({ "result": 1 }),
            block_height: 7,
        }
    }

    #[tokio::test]
    async fn an_unreachable_database_is_an_error_not_the_first_height() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_lazy("postgres://postgres@127.0.0.1:1/postgres")
            .unwrap();
        assert!(select_current_block_height(&pool, 10).await.is_err());
    }

    #[test]
    fn committed_results_leave_the_buffer_and_later_ones_stay() {
        let buffer = BitcoinRpcBuffer::default();
        let first = serde_json::json!({ "method": "getblockcount" });
        let later = serde_json::json!({ "method": "getbestblockhash" });
        buffer.lock().unwrap().insert(first.clone(), record("a"));

        let snapshot = snapshot_buffer(&buffer);
        let written: Vec<_> = snapshot
            .iter()
            .map(|(request, _)| request.clone())
            .collect();
        assert_eq!(written, vec![first.clone()]);

        // Until the commit succeeds the result stays visible to the source.
        assert!(buffer.lock().unwrap().contains_key(&first));

        // A result buffered while the commit ran survives it.
        buffer.lock().unwrap().insert(later.clone(), record("b"));
        remove_committed(&buffer, &written);
        let buffered = buffer.lock().unwrap();
        assert!(!buffered.contains_key(&first));
        assert!(buffered.contains_key(&later));
    }
}
