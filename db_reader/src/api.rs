use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct IndexTimes {
    pub fetch_time: u128,
    pub index_time: u128,
    pub commit_time: u128,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BRC20Tx {
    pub tx_id: String,
    pub inscription_id: String,
    pub inscription_number: i32,
    pub old_satpoint: Option<String>,
    pub new_satpoint: String,
    pub txid: String,
    pub new_pkscript: String,
    pub new_wallet: String,
    pub sent_as_fee: bool,
    pub content_hex: String,
    pub byte_len: u32,
    pub content_type_hex: String,
    pub parent_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BitmapInscription {
    pub tx_id: String,
    pub txid: String,
    pub inscription_id: String,
    pub inscription_number: i32,
    pub content_hex: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BlockInfo {
    pub block_hash: String,
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionInfo {
    pub _inscription_id: String,
    pub inscription_number: i32,
    pub cursed_for_brc20: bool,
    pub parent_id: Option<String>,
    pub is_json: bool,
    pub content_hex: String,
    pub content_type_hex: String,
    pub _metaprotocol_hex: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionEntry {
    pub charms: u16,
    pub id: String,
    pub inscription_number: i32,
    pub sequence_number: u32,
    pub is_json_or_text: bool,
    pub is_cursed_for_brc20: bool,
    pub txcnt_limit: i16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionInformation {
    pub info: InscriptionInfo,
    pub entry: InscriptionEntry,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UTXOInfo {
    pub sats: u64,
    pub sequence_numbers: Vec<u32>,
    pub satpoint_offsets: Vec<u64>,
}

#[rpc(server, client)]
pub trait Brc20Api {
    #[method(name = "getBlockIndexTimes")]
    async fn get_block_index_times(&self, block_height: u32) -> RpcResult<Option<IndexTimes>>;

    #[method(name = "getBlockBRC20Txes")]
    async fn get_block_brc20_txes(&self, block_height: u32) -> RpcResult<Option<Vec<BRC20Tx>>>;

    #[method(name = "getBlockHashAndTs")]
    async fn get_block_hash_and_ts(&self, block_height: u32) -> RpcResult<Option<BlockInfo>>;

    #[method(name = "getLatestBlockHeight")]
    async fn get_latest_block_height(&self) -> RpcResult<Option<u32>>;

    #[method(name = "getInscriptionInfo")]
    async fn get_inscription_info(
        &self,
        inscription_id: String,
    ) -> RpcResult<Option<InscriptionInformation>>;

    #[method(name = "getUTXOInfo")]
    async fn get_utxo_info(&self, outpoint: String) -> RpcResult<Option<UTXOInfo>>;

    #[method(name = "getInscriptionInfoBySequenceNumber")]
    async fn get_inscription_info_by_sequence_number(
        &self,
        sequence_number: u32,
    ) -> RpcResult<Option<InscriptionInformation>>;

    #[method(name = "getBlockBitmapInscrs")]
    async fn get_block_bitmap_inscrs(&self, block_height: u32) -> RpcResult<Option<Vec<BitmapInscription>>>;
}
