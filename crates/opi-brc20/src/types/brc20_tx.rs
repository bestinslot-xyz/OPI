/// One movement of an inscription that can carry a BRC-20, BRC2.0 program or
/// module operation.
///
/// `tx_id` is `<height>:<idx>`. `old_satpoint` is `None` for an inscribe.
/// `new_pkscript` is hex, `new_wallet` its address or an empty string, and
/// `byte_len` the length of the raw content.
#[derive(Debug, Clone)]
pub struct Brc20Tx {
    pub tx_id: String,
    pub inscription_id: String,
    pub inscription_number: i32,
    pub old_satpoint: Option<String>,
    pub new_satpoint: String,
    pub txid: String,
    pub new_pkscript: String,
    pub new_wallet: String,
    pub sent_as_fee: bool,
    pub content: serde_json::Value,
    pub byte_len: u32,
    pub parent_id: Option<String>,
}
