use std::collections::BTreeMap;

/// Block-level metadata for one finalized height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInfo {
    pub height: u32,
    pub hash: String,
    pub time: u32,
}

/// Static facts about one inscription, keyed by its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InscriptionInfo {
    pub inscription_id: String,
    pub inscription_number: i32,
    pub cursed_for_brc20: bool,
    pub parent_id: Option<String>,
    pub is_json: bool,
    pub content: Vec<u8>,
    pub content_type: Vec<u8>,
    pub metaprotocol: Vec<u8>,
}

/// One inscription movement within a block.
///
/// `idx` orders movements inside `height`. `old_satpoint` is `None` for
/// creations. Inscription ids use the `txidi<index>` form and satpoints the
/// `txid:vout:offset` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InscriptionRecord {
    pub height: u32,
    pub idx: u32,
    pub inscription_id: String,
    pub old_satpoint: Option<String>,
    pub new_satpoint: String,
    pub sent_as_fee: bool,
    pub new_output_value: u64,
    pub txid: String,
    pub new_pkscript: Vec<u8>,
}

/// A height and the block hash a module stored for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRef {
    pub height: u32,
    pub hash: String,
}

/// The inscription movements of one block in idx order, with the info of
/// every inscription they reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InscriptionFeed {
    pub records: Vec<InscriptionRecord>,
    pub infos: BTreeMap<String, InscriptionInfo>,
}

/// Everything a module needs to index one finalized block, read from a single
/// consistent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockView {
    pub height: u32,
    pub hash: String,
    pub time: u32,
    pub inscriptions: InscriptionFeed,
}

/// An indexer driven block by block by the scheduler.
#[async_trait::async_trait]
pub trait Module: Send {
    fn name(&self) -> &'static str;
    /// First height this module indexes on its network.
    fn start_height(&self) -> u32;
    /// Last block the module has applied.
    async fn cursor(&mut self) -> anyhow::Result<Option<BlockRef>>;
    /// Hash the module stored for `height`.
    async fn block_hash(&mut self, height: u32) -> anyhow::Result<Option<String>>;
    /// Drops every block above `height`.
    async fn rollback_to(&mut self, height: u32) -> anyhow::Result<()>;
    async fn apply(&mut self, block: &BlockView) -> anyhow::Result<()>;
}

/// Read access to indexed block data, independent of the backing store.
pub trait BlockSource: Send + Sync {
    /// Highest finalized height.
    fn tip(&self) -> anyhow::Result<Option<u32>>;
    fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>>;
    /// Records of `height` in idx order.
    fn inscription_records(&self, height: u32) -> anyhow::Result<Vec<InscriptionRecord>>;
    fn inscription_info(&self, inscription_id: &str) -> anyhow::Result<Option<InscriptionInfo>>;
    /// Header, records and referenced infos of `height` from one snapshot.
    /// `None` above the final tip.
    fn block_view(&self, height: u32) -> anyhow::Result<Option<BlockView>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSource;

    impl BlockSource for StubSource {
        fn tip(&self) -> anyhow::Result<Option<u32>> {
            Ok(Some(840_000))
        }

        fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>> {
            Ok(Some(BlockInfo {
                height,
                hash: "00".repeat(32),
                time: 1_700_000_000,
            }))
        }

        fn inscription_records(&self, _height: u32) -> anyhow::Result<Vec<InscriptionRecord>> {
            Ok(Vec::new())
        }

        fn inscription_info(
            &self,
            _inscription_id: &str,
        ) -> anyhow::Result<Option<InscriptionInfo>> {
            Ok(None)
        }

        fn block_view(&self, _height: u32) -> anyhow::Result<Option<BlockView>> {
            Ok(None)
        }
    }

    #[test]
    fn block_source_is_object_safe() {
        let source: Box<dyn BlockSource> = Box::new(StubSource);
        assert_eq!(source.tip().unwrap(), Some(840_000));
        assert_eq!(source.block_info(7).unwrap().unwrap().height, 7);
        assert!(source.inscription_records(7).unwrap().is_empty());
        assert!(source.inscription_info("x").unwrap().is_none());
    }
}
