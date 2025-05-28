use serde_json::Value as JsonValue;

#[derive(Debug)]
pub struct Transfer {
    pub tx_id: i64,
    pub inscription_id: String,
    pub old_satpoint: Option<String>,
    pub new_pkscript: String,
    pub new_wallet: Option<String>,
    pub sent_as_fee: bool,
    pub content: Option<JsonValue>,
    pub byte_length: i32,
    pub content_type: String,
    pub parent_inscription_id: Option<String>,
}
