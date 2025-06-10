use std::cmp::Ordering;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;

use hyper::Method;
use jsonrpsee::core::middleware::RpcServiceBuilder;
use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::server::{Server, ServerHandle};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned};
use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, DB};
use signal_hook::consts::SIGINT;
use signal_hook::iterator::Signals;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use serde::{Deserialize, Serialize};

struct RpcServer {
  pub db: &'static DB,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct IndexTimes {
  fetch_time: u128,
  index_time: u128,
  commit_time: u128,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BRC20Tx {
  tx_id: String,
  inscription_id: String,
  old_satpoint: Option<String>,
  new_pkscript: String,
  new_wallet: String,
  sent_as_fee: bool,
  content_hex: String,
  byte_len: u32,
  content_type_hex: String,
  parent_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BlockInfo {
  block_hash: String,
  timestamp: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionInfo {
  _inscription_id: String,
  _inscription_number: i32,
  cursed_for_brc20: bool,
  parent_id: Option<String>,
  is_json: bool,
  content_hex: String,
  content_type_hex: String,
  _metaprotocol_hex: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionEntry {
  charms: u16,
  id: String,
  inscription_number: i32,
  sequence_number: u32,
  is_json_or_text: bool,
  is_cursed_for_brc20: bool,
  txcnt_limit: i16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct InscriptionInformation {
  info: InscriptionInfo,
  entry: InscriptionEntry,
}

struct TransferInfo {
  inscription_id: String,
  old_satpoint: Option<String>,
  _new_satpoint: String,
  sent_as_fee: bool,
  _new_output_value: u64,
  new_pkscript: String,
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
  async fn get_inscription_info(&self, inscription_id: String) -> RpcResult<Option<InscriptionInformation>>;
}

pub fn wrap_rpc_error(error: Box<dyn Error>) -> ErrorObject<'static> {
  ErrorObjectOwned::owned(400, error.to_string(), None::<String>)
}

fn get_times_from_raw(
  raw: Option<Vec<u8>>,
) -> Option<IndexTimes> {
  if raw.is_none() {
    return None;
  }
  let raw = raw?;
  let mut iter = raw.chunks(16);
  let fetch_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  let index_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  let commit_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  Some(IndexTimes { fetch_time: fetch_tm, index_time: index_tm, commit_time: commit_tm })
}

fn load_inscription_id(raw: &[u8]) -> Option<String> {
  let mut rev_txid = Vec::new();
  for i in (0..32).rev() {
    rev_txid.push(raw[i]);
  }

  let txid = hex::encode(&rev_txid);
  if txid == "0000000000000000000000000000000000000000000000000000000000000000" {
    return None;
  }
  let index = u32::from_be_bytes(raw[32..36].try_into().unwrap());
  Some(format!("{}i{}", txid, index))
}

fn get_inscription_info_from_raw(
  raw: Vec<u8>,
  inscription_id: String,
) -> InscriptionInfo {
  let inscription_number = i32::from_be_bytes(raw[0..4].try_into().ok().unwrap());
  let cursed_for_brc20 = raw[4] != 0;
  let parent_id = load_inscription_id(&raw[5..41]);
  let is_json = raw[41] != 0;
  let content_len = u32::from_be_bytes(raw[42..46].try_into().ok().unwrap());
  let content_hex = hex::encode(&raw[46..(46 + content_len as usize)]);
  let content_type_len = u32::from_be_bytes(raw[(46 + content_len as usize)..(50 + content_len as usize)].try_into().ok().unwrap());
  let content_type_hex = hex::encode(&raw[(50 + content_len as usize)..(50 + content_len as usize + content_type_len as usize)]);
  let metaprotocol_len = u32::from_be_bytes(raw[(50 + content_len as usize + content_type_len as usize)..(54 + content_len as usize + content_type_len as usize)].try_into().ok().unwrap());
  let metaprotocol_hex = hex::encode(&raw[(54 + content_len as usize + content_type_len as usize)..(54 + content_len as usize + content_type_len as usize + metaprotocol_len as usize)]);
  InscriptionInfo {
    _inscription_id: inscription_id,
    _inscription_number: inscription_number,
    cursed_for_brc20,
    parent_id,
    is_json,
    content_hex,
    content_type_hex,
    _metaprotocol_hex: metaprotocol_hex,
  }
}

fn get_inscription_entry_from_raw(
  raw: Vec<u8>,
) -> InscriptionEntry {
  let charms = u16::from_be_bytes(raw[0..2].try_into().unwrap());
  let id = load_inscription_id(&raw[2..38]).unwrap();
  let inscription_number = i32::from_be_bytes(raw[38..42].try_into().unwrap());
  let sequence_number = u32::from_be_bytes(raw[42..46].try_into().unwrap());
  let is_json_or_text = raw[46] != 0;
  let is_cursed_for_brc20 = raw[47] != 0;
  let txcnt_limit = i16::from_be_bytes(raw[48..50].try_into().unwrap());

  InscriptionEntry {
    charms,
    id,
    inscription_number,
    sequence_number,
    is_json_or_text,
    is_cursed_for_brc20,
    txcnt_limit,
  }
}

fn load_satpoint(raw: &[u8]) -> Option<String> {
  let mut rev_txid = Vec::new();
  for i in (0..32).rev() {
    rev_txid.push(raw[i]);
  }
  let txid = hex::encode(&rev_txid);
  if txid == "0000000000000000000000000000000000000000000000000000000000000000" {
    return None;
  }
  let vout = u32::from_be_bytes(raw[32..36].try_into().unwrap());
  let sat = u64::from_be_bytes(raw[36..44].try_into().unwrap());
  Some(format!("{}:{}:{}", txid, vout, sat))
}

fn get_transfer_info_from_raw(
  raw: Vec<u8>,
) -> TransferInfo{
  let inscription_id = load_inscription_id(&raw[0..36]).unwrap();
  let old_satpoint = load_satpoint(&raw[36..80]);
  let new_satpoint = load_satpoint(&raw[80..124]).unwrap();
  let sent_as_fee = raw[124] != 0;
  let new_output_value = u64::from_be_bytes(raw[125..133].try_into().ok().unwrap());
  let new_pkscript = hex::encode(&raw[133..]);
  TransferInfo {
    inscription_id,
    old_satpoint,
    _new_satpoint: new_satpoint,
    sent_as_fee,
    _new_output_value: new_output_value,
    new_pkscript,
  }
}

fn compare_be_arrays(a: &[u8], b: &[u8]) -> Ordering {
  let min_len = a.len().min(b.len());
  for i in 0..min_len {
    let cmp = a[i].cmp(&b[i]);
    if cmp != Ordering::Equal {
      return cmp;
    }
  }
  a.len().cmp(&b.len())
}

fn is_valid_brc20(inscription_info: &InscriptionInfo) -> bool {
  if inscription_info.cursed_for_brc20 { return false; }
  if !inscription_info.is_json { return false; }

  let json_data: serde_json::Value = match serde_json::from_slice(&hex::decode(&inscription_info.content_hex).unwrap()) {
    Ok(data) => data,
    Err(_) => return false, // Invalid JSON
  };

  let p = json_data.get("p");
  if p.is_none() || !p.unwrap().is_string() {
    return false; // Missing or invalid 'p' field
  }
  let p_value = p.unwrap().as_str().unwrap();
  if p_value != "brc-20" && p_value != "brc20-prog" && p_value != "brc20-module" {
    return false;
  }
  if p_value == "brc20-module" {
    let module = json_data.get("module");
    if module.is_none() || !module.unwrap().is_string() {
      return false; // Missing or invalid 'module' field
    }
    let module_value = module.unwrap().as_str().unwrap();
    if module_value != "BRC20PROG" {
      return false; // Invalid module value
    }
  }
  
  return true;
}

fn get_wallet(pkscript: &str) -> String {
  bitcoin::Address::from_script(
    bitcoin::Script::from_bytes(&hex::decode(pkscript).unwrap()),
    bitcoin::Network::Bitcoin,
  )
  .map(|addr| addr.to_string())
  .unwrap_or_else(|_| "".to_string())
}

fn get_inscription_id_key(inscription_id: &str) -> Vec<u8> {
  let mut key = vec![0; 36];
  let txid = &inscription_id[0..64];
  let mut txid_dec = hex::decode(txid).unwrap();
  txid_dec.reverse(); // Reverse the txid to match the expected format
  let index = inscription_id[65..].parse::<u32>().unwrap();
  key[0..32].copy_from_slice(&txid_dec);
  key[32..36].copy_from_slice(&index.to_be_bytes());
  key
}

#[async_trait]
impl Brc20ApiServer for RpcServer {
  async fn get_block_index_times(
    &self,
    block_height: u32,
  ) -> RpcResult<Option<IndexTimes>> {
    let ord_index_stats = self.db.cf_handle("ord_index_stats")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'ord_index_stats' not found"))))?;

    Ok(self.db.get_cf(ord_index_stats, &block_height.to_be_bytes())
      .map(|time| get_times_from_raw(time))
      .unwrap())
  }

  async fn get_block_brc20_txes(
    &self,
    block_height: u32,
  ) -> RpcResult<Option<Vec<BRC20Tx>>> {
    let mut inscription_info_map = std::collections::HashMap::new();
    let mut invalid_brc20_map = std::collections::HashMap::new();

    let ord_transfers = self.db.cf_handle("ord_transfers")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'ord_transfers' not found"))))?;
    let ord_inscription_info = self.db.cf_handle("ord_inscription_info")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'ord_inscription_info' not found"))))?;

    // scan ord_transfers from block_height.0u32 to (block_height+1).0u32
    let start_key = block_height.to_be_bytes();
    let end_key = (block_height + 1).to_be_bytes();
    let mut iter = self.db.raw_iterator_cf(ord_transfers);
    iter.seek(start_key);
    let mut txes = Vec::new();
    while iter.valid() && compare_be_arrays(iter.key().unwrap(), &end_key) == Ordering::Less {
      let raw = iter.value().unwrap().to_vec();
      let transfer_info = get_transfer_info_from_raw(raw);

      let inscription_id = transfer_info.inscription_id.clone();
      if invalid_brc20_map.contains_key(&inscription_id) {
        iter.next();
        continue;
      }

      let inscription_info = if inscription_info_map.contains_key(&inscription_id) {
        inscription_info_map.get(&inscription_id).unwrap()
      } else {
        let inscription_id_key = get_inscription_id_key(&inscription_id);
        let raw_info = self.db.get_cf(ord_inscription_info, &inscription_id_key).unwrap().unwrap();
        let info = get_inscription_info_from_raw(raw_info, inscription_id.clone());
        inscription_info_map.insert(inscription_id.clone(), info.clone());

        &info.clone()
      };

      if !is_valid_brc20(inscription_info) {
        invalid_brc20_map.insert(inscription_id, ());
        iter.next();
        continue;
      }

      let block_height = u32::from_be_bytes(iter.key().unwrap()[0..4].try_into().unwrap());
      let tx_index = u32::from_be_bytes(iter.key().unwrap()[4..8].try_into().unwrap());
      let tx_id = format!(
        "{}:{}",
        block_height,
        tx_index
      );

      txes.push(BRC20Tx {
        tx_id,
        inscription_id,
        old_satpoint: transfer_info.old_satpoint,
        new_pkscript: transfer_info.new_pkscript.clone(),
        new_wallet: get_wallet(&transfer_info.new_pkscript),
        sent_as_fee: transfer_info.sent_as_fee,
        content_hex: inscription_info.content_hex.clone(),
        byte_len: inscription_info.content_hex.len() as u32 / 2, // Each byte is represented by 2 hex characters
        content_type_hex: inscription_info.content_type_hex.clone(),
        parent_id: inscription_info.parent_id.clone(),
      });

      iter.next();
    }

    Ok(Some(txes))
  }

  async fn get_block_hash_and_ts(
    &self,
    block_height: u32,
  ) -> RpcResult<Option<BlockInfo>> {
    let height_to_block_header = self.db.cf_handle("height_to_block_header")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'height_to_block_header' not found"))))?;

    let key = block_height.to_be_bytes();
    if let Some(raw) = self.db.get_cf(height_to_block_header, &key).unwrap() {
      let header: bitcoin::block::Header = bitcoin::consensus::encode::deserialize(&raw).unwrap();
      let hash = header.block_hash().to_string();
      let timestamp = header.time as u64;
      Ok(Some(BlockInfo { block_hash: hash, timestamp }))
    } else {
      Ok(None)
    }
  }

  async fn get_latest_block_height(&self) -> RpcResult<Option<u32>> {
    let height_to_block_header = self.db.cf_handle("height_to_block_header")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'height_to_block_header' not found"))))?;

    let block_height = self.db.iterator_cf(height_to_block_header, IteratorMode::End)
      .next()
      .transpose()
      .unwrap_or(None)
      .map(|(height, _header)| u32::from_be_bytes((*height).try_into().unwrap()));

    Ok(block_height)
  }

  async fn get_inscription_info(
    &self,
    inscription_id: String,
  ) -> RpcResult<Option<InscriptionInformation>> {
    let ord_inscription_info = self.db.cf_handle("ord_inscription_info")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'ord_inscription_info' not found"))))?;
    let inscription_id_to_sequence_number = self.db.cf_handle("inscription_id_to_sequence_number")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'inscription_id_to_sequence_number' not found"))))?;
    let sequence_number_to_inscription_entry = self.db.cf_handle("sequence_number_to_inscription_entry")
      .ok_or_else(|| wrap_rpc_error(Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Column family 'sequence_number_to_inscription_entry' not found"))))?;

    let inscription_id_key = get_inscription_id_key(&inscription_id);
    // Check if the inscription_id exists in the inscription_id_to_sequence_number column family
    let sequence_number_raw = self.db.get_cf(inscription_id_to_sequence_number, &inscription_id_key).unwrap();
    if sequence_number_raw.is_none() {
      return Ok(None);
    }
    let sequence_number = u32::from_be_bytes(sequence_number_raw.unwrap()[0..4].try_into().unwrap());

    // Now check if the sequence_number exists in the sequence_number_to_inscription_entry column family
    let entry_raw = self.db.get_cf(sequence_number_to_inscription_entry, &sequence_number.to_be_bytes()).unwrap();
    if entry_raw.is_none() {
      return Ok(None);
    }
    // If both checks passed, we can retrieve the inscription info
    let entry = get_inscription_entry_from_raw(entry_raw.unwrap().to_vec());

    if let Some(raw) = self.db.get_cf(ord_inscription_info, &inscription_id_key).unwrap() {
      let info = get_inscription_info_from_raw(raw, inscription_id.clone());
      
      Ok(Some(InscriptionInformation { info, entry }))
    } else {
      Ok(None)
    }
  }
}

pub async fn start_rpc_server(
  db: &'static DB,
) -> Result<ServerHandle, Box<dyn Error>> {
  let cors = CorsLayer::new()
    // Allow `POST` when accessing the resource
    .allow_methods([Method::POST])
    // Allow requests from any origin
    .allow_origin(Any)
    .allow_headers([hyper::header::CONTENT_TYPE]);

  let http_middleware =
    ServiceBuilder::new()
      .layer(cors);
  let rpc_middleware = RpcServiceBuilder::new()
    .rpc_logger(1024);
  let module = RpcServer { 
    db,
  }.into_rpc();

  let handle = Server::builder()
    .set_http_middleware(http_middleware)
    .set_rpc_middleware(rpc_middleware)
    .build("0.0.0.0:11030".parse::<SocketAddr>()?)
    .await?
    .start(module);

  println!("RPC server started at http://0.0.0.0:11030");

  Ok(handle)
}

#[tokio::main]
async fn main() {
  rlimit::Resource::NOFILE.set(4096, 8192)
    .expect("Failed to set file descriptor limits");
  let mut signals = Signals::new([SIGINT])
    .expect("Failed to create signal handler");

  let index_path = PathBuf::from("../../../ord/target/release/dbs");

  let column_families = vec! [
    ColumnFamilyDescriptor::new("height_to_block_header", Options::default()),
    ColumnFamilyDescriptor::new("inscription_id_to_sequence_number", Options::default()),
    ColumnFamilyDescriptor::new("sequence_number_to_inscription_entry", Options::default()),
    ColumnFamilyDescriptor::new("ord_transfers", Options::default()),
    ColumnFamilyDescriptor::new("ord_inscription_info", Options::default()),
    ColumnFamilyDescriptor::new("ord_index_stats", Options::default()),
  ];
  
  let db_path = index_path.join("index.db");
  let sec_db_path = index_path.join("secondary.db");
  let db = DB::open_cf_descriptors_as_secondary(&Options::default(), &db_path, &sec_db_path, column_families)
    .expect("Failed to open database");

  // Leak the DB to get a 'static reference
  let db: &'static DB = Box::leak(Box::new(db));

  let rpc_handle = start_rpc_server(db).await.unwrap();

  tokio::spawn(rpc_handle.stopped());

  // The server will run indefinitely, handling requests.
  // You can add more functionality or shutdown logic as needed.
  // For now, we just keep the main function running.
  loop {
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    if signals.pending().next().is_some() {
      println!("Received SIGINT, stopping RPC server...");
      break; // Exit the loop on SIGINT
    }
    
    db.try_catch_up_with_primary()
      .map_err(|e| eprintln!("Failed to catch up with primary: {}", e))
      .ok();
  }

  println!("RPC server stopped.");
}
