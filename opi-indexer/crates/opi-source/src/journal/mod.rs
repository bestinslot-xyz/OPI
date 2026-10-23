mod schema;

pub use schema::*;

use {
    anyhow::{Result, bail, ensure},
    ord::Chain,
    rocksdb::{ColumnFamilyDescriptor, DB, Options},
    std::{path::Path, sync::Arc},
};

/// Column family names.
pub mod cf {
    pub const TRANSFERS: &str = "ord_transfers";
    pub const INFO: &str = "ord_inscription_info";
    pub const HEADERS: &str = "height_to_block_header";
    pub const RAW_EVENTS: &str = "raw_events";
    pub const RAW_HEIGHTS: &str = "raw_heights";
    pub const META: &str = "meta";
}

const ALL_CFS: [&str; 6] = [
    cf::TRANSFERS,
    cf::INFO,
    cf::HEADERS,
    cf::RAW_EVENTS,
    cf::RAW_HEIGHTS,
    cf::META,
];

const META_FINAL_TIP: &[u8] = b"final_tip";
const META_CHAIN: &[u8] = b"chain";
const META_SCHEMA: &[u8] = b"schema";
const SCHEMA_VERSION: u32 = 1;

/// RocksDB-backed journal of finalized ord events.
pub struct Journal {
    db: Arc<DB>,
}

impl Journal {
    /// Opens the journal at `path`, creating it for `chain` when absent.
    /// Fails when an existing journal belongs to another chain.
    pub fn open(path: &Path, chain: Chain) -> Result<Journal> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let descriptors = ALL_CFS
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()));
        let db = Arc::new(DB::open_cf_descriptors(&options, path, descriptors)?);
        let journal = Journal { db };
        let meta = journal.meta()?;
        let chain = chain.to_string();
        match journal.db.get_cf(&meta, META_CHAIN)? {
            Some(existing) => {
                if existing != chain.as_bytes() {
                    bail!(
                        "journal chain mismatch: journal is {}, requested {chain}",
                        String::from_utf8_lossy(&existing)
                    );
                }
            }
            None => {
                journal.db.put_cf(&meta, META_CHAIN, chain.as_bytes())?;
                journal
                    .db
                    .put_cf(&meta, META_SCHEMA, SCHEMA_VERSION.to_be_bytes())?;
            }
        }
        Ok(journal)
    }

    /// Opens an existing journal without write access.
    pub fn open_read_only(path: &Path) -> Result<Journal> {
        let db = DB::open_cf_for_read_only(&Options::default(), path, ALL_CFS, false)?;
        let journal = Journal { db: Arc::new(db) };
        let meta = journal.meta()?;
        let schema = journal.db.get_cf(&meta, META_SCHEMA)?;
        ensure!(
            schema.as_deref() == Some(&SCHEMA_VERSION.to_be_bytes()[..]),
            "unsupported journal schema"
        );
        Ok(journal)
    }

    /// Highest finalized height, if any block has been finalized.
    pub fn final_tip(&self) -> Result<Option<u32>> {
        let meta = self.meta()?;
        let Some(raw) = self.db.get_cf(&meta, META_FINAL_TIP)? else {
            return Ok(None);
        };
        let bytes: [u8; 4] = raw
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("final_tip is {} bytes, expected 4", raw.len()))?;
        Ok(Some(u32::from_be_bytes(bytes)))
    }

    pub fn db(&self) -> &Arc<DB> {
        &self.db
    }

    fn meta(&self) -> Result<&rocksdb::ColumnFamily> {
        self.db
            .cf_handle(cf::META)
            .ok_or_else(|| anyhow::anyhow!("missing column family {}", cf::META))
    }
}
