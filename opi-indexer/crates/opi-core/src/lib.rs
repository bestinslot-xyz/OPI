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

/// Read access to indexed block data, independent of the backing store.
pub trait BlockSource: Send + Sync {
    /// Highest finalized height.
    fn tip(&self) -> anyhow::Result<Option<u32>>;
    fn block_info(&self, height: u32) -> anyhow::Result<Option<BlockInfo>>;
    /// Records of `height` in idx order.
    fn inscription_records(&self, height: u32) -> anyhow::Result<Vec<InscriptionRecord>>;
    fn inscription_info(&self, inscription_id: &str) -> anyhow::Result<Option<InscriptionInfo>>;
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
