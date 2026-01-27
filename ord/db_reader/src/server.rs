use std::cmp::Ordering;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::{
  BRC20Tx, BitmapInscription, BlockInfo, Brc20ApiServer, IndexTimes, InscriptionEntry,
  InscriptionInfo, InscriptionInformation, SNSInscription, UTXOInfo,
};
use bitcoin::Network::{self};
use hyper::Method;
use jsonrpsee::core::middleware::RpcServiceBuilder;
use jsonrpsee::core::{RpcResult, async_trait};
use jsonrpsee::server::Server;
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned};
use parking_lot::ReentrantMutex;
use rocksdb::{DB, IteratorMode};
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};

use crate::Config;

struct RpcServer {
  db: Arc<ReentrantMutex<DB>>,
  network: Network,
}

fn wrap_rpc_error(error: Box<dyn Error>) -> ErrorObject<'static> {
  ErrorObjectOwned::owned(400, error.to_string(), None::<String>)
}

fn get_times_from_raw(raw: Option<Vec<u8>>) -> Option<IndexTimes> {
  if raw.is_none() {
    return None;
  }
  let raw = raw?;
  let mut iter = raw.chunks(16);
  let fetch_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  let index_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  let commit_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
  Some(IndexTimes {
    fetch_time: fetch_tm,
    index_time: index_tm,
    commit_time: commit_tm,
  })
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

fn get_inscription_info_from_raw(raw: Vec<u8>, inscription_id: String) -> InscriptionInfo {
  let inscription_number = i32::from_be_bytes(raw[0..4].try_into().ok().unwrap());
  let cursed_for_brc20 = raw[4] != 0;
  let parent_id = load_inscription_id(&raw[5..41]);
  let is_json = raw[41] != 0;
  let content_len = u32::from_be_bytes(raw[42..46].try_into().ok().unwrap());
  let content_hex = hex::encode(&raw[46..(46 + content_len as usize)]);
  let content_type_len = u32::from_be_bytes(
    raw[(46 + content_len as usize)..(50 + content_len as usize)]
      .try_into()
      .ok()
      .unwrap(),
  );
  let content_type_hex = hex::encode(
    &raw[(50 + content_len as usize)..(50 + content_len as usize + content_type_len as usize)],
  );
  let metaprotocol_len = u32::from_be_bytes(
    raw[(50 + content_len as usize + content_type_len as usize)
      ..(54 + content_len as usize + content_type_len as usize)]
      .try_into()
      .ok()
      .unwrap(),
  );
  let metaprotocol_hex = hex::encode(
    &raw[(54 + content_len as usize + content_type_len as usize)
      ..(54 + content_len as usize + content_type_len as usize + metaprotocol_len as usize)],
  );
  InscriptionInfo {
    _inscription_id: inscription_id,
    inscription_number,
    cursed_for_brc20,
    parent_id,
    is_json,
    content_hex,
    content_type_hex,
    _metaprotocol_hex: metaprotocol_hex,
  }
}

fn get_inscription_entry_from_raw(raw: Vec<u8>) -> InscriptionEntry {
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

fn varint_decode(buffer: &[u8]) -> Result<(u128, usize), VarintError> {
  let mut n = 0u128;

  for (i, &byte) in buffer.iter().enumerate() {
    if i > 18 {
      return Err(VarintError::Overlong);
    }

    let value = u128::from(byte) & 0b0111_1111;

    if i == 18 && value & 0b0111_1100 != 0 {
      return Err(VarintError::Overflow);
    }

    n |= value << (7 * i);

    if byte & 0b1000_0000 == 0 {
      return Ok((n, i + 1));
    }
  }

  Err(VarintError::Unterminated)
}

#[derive(PartialEq, Debug)]
pub enum VarintError {
  Overlong,
  Overflow,
  Unterminated,
}

impl Display for VarintError {
  fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
    match self {
      Self::Overlong => write!(f, "too long"),
      Self::Overflow => write!(f, "overflow"),
      Self::Unterminated => write!(f, "unterminated"),
    }
  }
}

impl std::error::Error for VarintError {}

fn get_utxo_entry_from_raw(raw: Vec<u8>) -> UTXOInfo {
  let sats;

  let mut offset = 0;
  let (value, varint_len) = varint_decode(&raw).unwrap();
  sats = value as u64;
  offset += varint_len;

  let mut parsed_inscriptions = Vec::new();
  while offset < raw.len() {
    let sequence_number = u32::from_be_bytes(raw[offset..offset + 4].try_into().unwrap());
    offset += 4;

    let (satpoint_offset, varint_len) = varint_decode(&raw[offset..]).unwrap();
    let satpoint_offset = u64::try_from(satpoint_offset).unwrap();
    offset += varint_len;

    parsed_inscriptions.push((sequence_number, satpoint_offset));
  }

  let sequence_numbers: Vec<u32> = parsed_inscriptions.iter().map(|(seq, _)| *seq).collect();
  let satpoint_offsets: Vec<u64> = parsed_inscriptions
    .iter()
    .map(|(_, offset)| *offset)
    .collect();
  UTXOInfo {
    sats,
    sequence_numbers,
    satpoint_offsets,
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
  let vout = u32::from_le_bytes(raw[32..36].try_into().unwrap());
  let sat = u64::from_le_bytes(raw[36..44].try_into().unwrap());
  Some(format!("{}:{}:{}", txid, vout, sat))
}

fn load_txid(raw: &[u8]) -> String {
  let mut rev_txid = Vec::new();
  for i in (0..32).rev() {
    rev_txid.push(raw[i]);
  }
  hex::encode(&rev_txid)
}

struct TransferInfo {
  inscription_id: String,
  old_satpoint: Option<String>,
  new_satpoint: String,
  sent_as_fee: bool,
  _new_output_value: u64,
  txid: String,
  new_pkscript: String,
}

fn get_transfer_info_from_raw(raw: Vec<u8>) -> TransferInfo {
  let inscription_id = load_inscription_id(&raw[0..36]).unwrap();
  let old_satpoint = load_satpoint(&raw[36..80]);
  let new_satpoint = load_satpoint(&raw[80..124]).unwrap();
  let sent_as_fee = raw[124] != 0;
  let new_output_value = u64::from_be_bytes(raw[125..133].try_into().ok().unwrap());
  let txid = load_txid(&raw[133..165]);
  let new_pkscript = hex::encode(&raw[165..]);
  TransferInfo {
    inscription_id,
    old_satpoint,
    new_satpoint,
    sent_as_fee,
    _new_output_value: new_output_value,
    txid,
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
  if inscription_info.cursed_for_brc20 {
    return false;
  }
  if !inscription_info.is_json {
    return false;
  }

  let Ok(content_type) = hex::decode(&inscription_info.content_type_hex) else {
    return false;
  };

  let Ok(decoded_content_type) = String::from_utf8(content_type) else {
    return false;
  };

  if !decoded_content_type.eq("text/plain")
    && !decoded_content_type.starts_with("text/plain;")
    && !decoded_content_type.eq("application/json")
    && !decoded_content_type.starts_with("application/json;")
  {
    return false;
  }

  let json_data: serde_json::Value =
    match serde_json::from_slice(&hex::decode(&inscription_info.content_hex).unwrap()) {
      Ok(data) => data,
      Err(_) => return false, // Invalid JSON
    };

  let Some(protocol) = json_data.get("p").and_then(|v| v.as_str()) else {
    return false; // Missing or invalid 'p' field
  };
  if protocol != "brc-20" && protocol != "brc20-prog" && protocol != "brc20-module" {
    return false;
  }
  if protocol == "brc20-module" {
    let Some(module) = json_data.get("module").and_then(|m| m.as_str()) else {
      return false; // Missing or invalid 'module' field for brc20-module
    };
    if module != "BRC20PROG" {
      return false; // Invalid module value
    }
  }

  return true;
}

fn is_valid_bitmap(inscription_info: &InscriptionInfo) -> bool {
  if inscription_info.inscription_number < 0 {
    return false;
  }
  if inscription_info.is_json {
    return false;
  }
  if !inscription_info
    .content_type_hex
    .to_lowercase()
    .starts_with("746578742f706c61696e")
  {
    return false;
  }

  true
}

fn is_valid_sns(inscription_info: &InscriptionInfo) -> bool {
  if inscription_info.inscription_number < 0 {
    return false;
  }
  if !inscription_info
    .content_type_hex
    .to_lowercase()
    .starts_with("746578742f706c61696e")
    && !inscription_info
      .content_type_hex
      .to_lowercase()
      .starts_with("6170706c69636174696f6e2f6a736f6e")
  {
    return false;
  }

  true
}

fn get_wallet(pkscript: &str, network: Network) -> String {
  bitcoin::Address::from_script(
    bitcoin::Script::from_bytes(&hex::decode(pkscript).unwrap()),
    network,
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

fn parse_outpoint(outpoint: &str) -> (Vec<u8>, u32) {
  let parts: Vec<&str> = outpoint.split(':').collect();
  if parts.len() != 2 {
    panic!("Invalid outpoint format, expected <txid>:<vout>");
  }
  let mut txid = hex::decode(parts[0]).unwrap();
  txid.reverse(); // Reverse the txid to match the expected format
  let vout = parts[1].parse::<u32>().unwrap();
  (txid, vout)
}

fn get_outpoint_key(outpoint: &str) -> Vec<u8> {
  let mut key = vec![0; 36];
  let (txid, vout) = parse_outpoint(outpoint);
  key[0..32].copy_from_slice(&txid);
  key[32..36].copy_from_slice(&vout.to_le_bytes());
  key
}

#[async_trait]
impl Brc20ApiServer for RpcServer {
  async fn get_block_index_times(&self, block_height: u32) -> RpcResult<Option<IndexTimes>> {
    let db = self.db.lock();
    let ord_index_stats = db.cf_handle("ord_index_stats").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_index_stats' not found",
      )))
    })?;

    Ok(
      db.get_cf(ord_index_stats, &block_height.to_be_bytes())
        .map(|time| get_times_from_raw(time))
        .unwrap(),
    )
  }

  async fn get_block_brc20_txes(&self, block_height: u32) -> RpcResult<Option<Vec<BRC20Tx>>> {
    let mut inscription_info_map = std::collections::HashMap::new();
    let mut invalid_brc20_map = std::collections::HashMap::new();

    let db = self.db.lock();
    let ord_transfers = db.cf_handle("ord_transfers").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_transfers' not found",
      )))
    })?;
    let ord_inscription_info = db.cf_handle("ord_inscription_info").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_inscription_info' not found",
      )))
    })?;

    // scan ord_transfers from block_height.0u32 to (block_height+1).0u32
    let start_key = block_height.to_be_bytes();
    let end_key = (block_height + 1).to_be_bytes();
    let mut iter = db.raw_iterator_cf(ord_transfers);
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
        let raw_info = db
          .get_cf(ord_inscription_info, &inscription_id_key)
          .unwrap()
          .unwrap();
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
      let tx_id = format!("{}:{}", block_height, tx_index);

      txes.push(BRC20Tx {
        tx_id,
        inscription_id,
        inscription_number: inscription_info.inscription_number,
        old_satpoint: transfer_info.old_satpoint,
        new_satpoint: transfer_info.new_satpoint,
        txid: transfer_info.txid,
        new_pkscript: transfer_info.new_pkscript.clone(),
        new_wallet: get_wallet(&transfer_info.new_pkscript, self.network),
        sent_as_fee: transfer_info.sent_as_fee,
        content: serde_json::from_slice(
          hex::decode(&inscription_info.content_hex)
            .unwrap_or(vec![])
            .as_slice(),
        )
        .unwrap_or(serde_json::Value::Null),
        byte_len: inscription_info.content_hex.len() as u32 / 2, // Each byte is represented by 2 hex characters
        parent_id: inscription_info.parent_id.clone(),
      });

      iter.next();
    }

    Ok(Some(txes))
  }

  async fn get_block_hash_and_ts(&self, block_height: u32) -> RpcResult<Option<BlockInfo>> {
    let db = self.db.lock();
    let height_to_block_header = db.cf_handle("height_to_block_header").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'height_to_block_header' not found",
      )))
    })?;

    let key = block_height.to_be_bytes();
    if let Some(raw) = db.get_cf(height_to_block_header, &key).unwrap() {
      let header: bitcoin::block::Header = bitcoin::consensus::encode::deserialize(&raw).unwrap();
      let hash = header.block_hash().to_string();
      let timestamp = header.time as u64;
      Ok(Some(BlockInfo {
        block_hash: hash,
        timestamp,
      }))
    } else {
      Ok(None)
    }
  }

  async fn get_latest_block_height(&self) -> RpcResult<Option<u32>> {
    let db = self.db.lock();
    let height_to_block_header = db.cf_handle("height_to_block_header").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'height_to_block_header' not found",
      )))
    })?;

    let block_height = db
      .iterator_cf(height_to_block_header, IteratorMode::End)
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
    let db = self.db.lock();
    let ord_inscription_info = db.cf_handle("ord_inscription_info").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_inscription_info' not found",
      )))
    })?;
    let inscription_id_to_sequence_number = db
      .cf_handle("inscription_id_to_sequence_number")
      .ok_or_else(|| {
        wrap_rpc_error(Box::new(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          "Column family 'inscription_id_to_sequence_number' not found",
        )))
      })?;
    let sequence_number_to_inscription_entry = db
      .cf_handle("sequence_number_to_inscription_entry")
      .ok_or_else(|| {
        wrap_rpc_error(Box::new(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          "Column family 'sequence_number_to_inscription_entry' not found",
        )))
      })?;

    let inscription_id_key = get_inscription_id_key(&inscription_id);
    // Check if the inscription_id exists in the inscription_id_to_sequence_number column family
    let sequence_number_raw = db
      .get_cf(inscription_id_to_sequence_number, &inscription_id_key)
      .unwrap();
    if sequence_number_raw.is_none() {
      return Ok(None);
    }
    let sequence_number =
      u32::from_be_bytes(sequence_number_raw.unwrap()[0..4].try_into().unwrap());

    // Now check if the sequence_number exists in the sequence_number_to_inscription_entry column family
    let entry_raw = self
      .db
      .lock()
      .get_cf(
        sequence_number_to_inscription_entry,
        &sequence_number.to_be_bytes(),
      )
      .unwrap();
    if entry_raw.is_none() {
      return Ok(None);
    }
    // If both checks passed, we can retrieve the inscription info
    let entry = get_inscription_entry_from_raw(entry_raw.unwrap().to_vec());

    if let Some(raw) = db
      .get_cf(ord_inscription_info, &inscription_id_key)
      .unwrap()
    {
      let info = get_inscription_info_from_raw(raw, inscription_id.clone());

      Ok(Some(InscriptionInformation { info, entry }))
    } else {
      Ok(None)
    }
  }

  async fn get_utxo_info(&self, outpoint: String) -> RpcResult<Option<UTXOInfo>> {
    let db = self.db.lock();
    let outpoint_to_utxo_entry = db.cf_handle("outpoint_to_utxo_entry").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'outpoint_to_utxo_entry' not found",
      )))
    })?;

    let outpoint_key = get_outpoint_key(&outpoint);

    if let Some(raw) = db.get_cf(outpoint_to_utxo_entry, &outpoint_key).unwrap() {
      let utxo_info = get_utxo_entry_from_raw(raw.to_vec());
      Ok(Some(utxo_info))
    } else {
      Ok(None)
    }
  }

  async fn get_inscription_info_by_sequence_number(
    &self,
    sequence_number: u32,
  ) -> RpcResult<Option<InscriptionInformation>> {
    let db = self.db.lock();
    let ord_inscription_info = db.cf_handle("ord_inscription_info").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_inscription_info' not found",
      )))
    })?;
    let sequence_number_to_inscription_entry = db
      .cf_handle("sequence_number_to_inscription_entry")
      .ok_or_else(|| {
        wrap_rpc_error(Box::new(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          "Column family 'sequence_number_to_inscription_entry' not found",
        )))
      })?;

    let entry_raw = db
      .get_cf(
        sequence_number_to_inscription_entry,
        &sequence_number.to_be_bytes(),
      )
      .unwrap();
    if entry_raw.is_none() {
      return Ok(None);
    }
    let entry = get_inscription_entry_from_raw(entry_raw.unwrap().to_vec());

    let inscription_id_key = get_inscription_id_key(&entry.id);
    if let Some(raw) = self
      .db
      .lock()
      .get_cf(ord_inscription_info, &inscription_id_key)
      .unwrap()
    {
      let info = get_inscription_info_from_raw(raw, entry.id.clone());

      Ok(Some(InscriptionInformation { info, entry }))
    } else {
      Ok(None)
    }
  }

  async fn get_block_bitmap_inscrs(
    &self,
    block_height: u32,
  ) -> RpcResult<Option<Vec<BitmapInscription>>> {
    let db = self.db.lock();
    let ord_transfers = db.cf_handle("ord_transfers").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_transfers' not found",
      )))
    })?;
    let ord_inscription_info = db.cf_handle("ord_inscription_info").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_inscription_info' not found",
      )))
    })?;

    // scan ord_transfers from block_height.0u32 to (block_height+1).0u32
    let start_key = block_height.to_be_bytes();
    let end_key = (block_height + 1).to_be_bytes();
    let mut iter = db.raw_iterator_cf(ord_transfers);
    iter.seek(start_key);
    let mut bitmap_inscrs = Vec::new();
    while iter.valid() && compare_be_arrays(iter.key().unwrap(), &end_key) == Ordering::Less {
      let raw = iter.value().unwrap().to_vec();
      let transfer_info = get_transfer_info_from_raw(raw);

      if transfer_info.old_satpoint.is_some() {
        // This is a transfer, skip it
        iter.next();
        continue;
      }

      let inscription_id = transfer_info.inscription_id.clone();

      let inscription_id_key = get_inscription_id_key(&inscription_id);
      let raw_info = db
        .get_cf(ord_inscription_info, &inscription_id_key)
        .unwrap()
        .unwrap();
      let info = get_inscription_info_from_raw(raw_info, inscription_id.clone());
      let inscription_info = info.clone();

      if !is_valid_bitmap(&inscription_info) {
        iter.next();
        continue;
      }

      let block_height = u32::from_be_bytes(iter.key().unwrap()[0..4].try_into().unwrap());
      let tx_index = u32::from_be_bytes(iter.key().unwrap()[4..8].try_into().unwrap());
      let tx_id = format!("{}:{}", block_height, tx_index);

      bitmap_inscrs.push(BitmapInscription {
        tx_id,
        inscription_id,
        inscription_number: inscription_info.inscription_number,
        txid: transfer_info.txid,
        content_hex: inscription_info.content_hex.clone(),
      });

      iter.next();
    }

    // sort bitmap_inscrs by inscription_number
    bitmap_inscrs.sort_by(|a, b| a.inscription_number.cmp(&b.inscription_number));

    Ok(Some(bitmap_inscrs))
  }

  async fn get_block_sns_inscrs(
    &self,
    block_height: u32,
  ) -> RpcResult<Option<Vec<SNSInscription>>> {
    let db = self.db.lock();
    let ord_transfers = db.cf_handle("ord_transfers").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_transfers' not found",
      )))
    })?;
    let ord_inscription_info = db.cf_handle("ord_inscription_info").ok_or_else(|| {
      wrap_rpc_error(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Column family 'ord_inscription_info' not found",
      )))
    })?;

    // scan ord_transfers from block_height.0u32 to (block_height+1).0u32
    let start_key = block_height.to_be_bytes();
    let end_key = (block_height + 1).to_be_bytes();
    let mut iter = db.raw_iterator_cf(ord_transfers);
    iter.seek(start_key);
    let mut sns_inscrs = Vec::new();
    while iter.valid() && compare_be_arrays(iter.key().unwrap(), &end_key) == Ordering::Less {
      let raw = iter.value().unwrap().to_vec();
      let transfer_info = get_transfer_info_from_raw(raw);

      if transfer_info.old_satpoint.is_some() {
        // This is a transfer, skip it
        iter.next();
        continue;
      }

      let inscription_id = transfer_info.inscription_id.clone();

      let inscription_id_key = get_inscription_id_key(&inscription_id);
      let raw_info = db
        .get_cf(ord_inscription_info, &inscription_id_key)
        .unwrap()
        .unwrap();
      let info = get_inscription_info_from_raw(raw_info, inscription_id.clone());
      let inscription_info = info.clone();

      if !is_valid_sns(&inscription_info) {
        iter.next();
        continue;
      }

      let block_height = u32::from_be_bytes(iter.key().unwrap()[0..4].try_into().unwrap());
      let tx_index = u32::from_be_bytes(iter.key().unwrap()[4..8].try_into().unwrap());
      let tx_id = format!("{}:{}", block_height, tx_index);

      sns_inscrs.push(SNSInscription {
        tx_id,
        inscription_id,
        inscription_number: inscription_info.inscription_number,
        txid: transfer_info.txid,
        content_hex: inscription_info.content_hex.clone(),
        content_type_hex: inscription_info.content_type_hex.clone(),
      });

      iter.next();
    }

    // sort sns_inscrs by inscription_number
    sns_inscrs.sort_by(|a, b| a.inscription_number.cmp(&b.inscription_number));

    Ok(Some(sns_inscrs))
  }
}

pub async fn start_rpc_server(
  config: Config,
  db: Arc<ReentrantMutex<DB>>,
) -> Result<(), Box<dyn Error>> {
  let cors = CorsLayer::new()
        // Allow `POST` when accessing the resource
        .allow_methods([Method::POST])
        // Allow requests from any origin
        .allow_origin(Any)
        .allow_headers([hyper::header::CONTENT_TYPE]);

  let http_middleware = ServiceBuilder::new().layer(cors);
  let rpc_middleware = RpcServiceBuilder::new().rpc_logger(1024);
  let module = RpcServer {
    db,
    network: config.network,
  }
  .into_rpc();

  let server_config = jsonrpsee::server::ServerConfig::builder()
        .max_request_body_size(1024 * 1024 * 100) // 100 MB
        .max_response_body_size(1024 * 1024 * 1024) // 1 GB
        .build();

  let url = config
    .api_url
    .clone()
    .unwrap_or_else(|| "127.0.0.1:11030".into());
  let handle = Server::builder()
    .set_http_middleware(http_middleware)
    .set_rpc_middleware(rpc_middleware)
    .set_config(server_config)
    .build(url.parse::<SocketAddr>()?)
    .await?
    .start(module);

  println!("RPC server started at http://{}", url);

  handle.stopped().await;

  println!("RPC server stopped.");
  Ok(())
}
