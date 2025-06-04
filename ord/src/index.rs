use {
  self::{
    entry::{
      Entry, InscriptionEntry,
      SatRange,
    },
    event::Event,
    reorg::Reorg,
    updater::Updater,
    utxo_entry::{ParsedUtxoEntry, UtxoEntryBuf},
  },
  super::*,
  bitcoin::block::Header,
  bitcoincore_rpc::{
    Client,
  },
  indicatif::{ProgressBar, ProgressStyle},
  log::log_enabled,
  std::{
    collections::HashMap,
  },
  rocksdb::{DB, Options, IteratorMode},
};

pub use updater::get_tx_limits;

pub(crate) mod entry;
pub mod event;
mod fetcher;
mod lot;
mod reorg;
mod updater;
mod utxo_entry;

const SCHEMA_VERSION: u64 = 99100030;

#[derive(Copy, Clone)]
pub(crate) enum Statistic {
  Schema = 0,
  BlessedInscriptions = 1,
  CursedInscriptions = 3,
}

impl Statistic {
  fn key(self) -> u64 {
    self.into()
  }
}

impl From<Statistic> for u64 {
  fn from(statistic: Statistic) -> Self {
    statistic as u64
  }
}

#[derive(Serialize)]
pub struct Info {
  blocks_indexed: u32,
  branch_pages: u64,
  fragmented_bytes: u64,
  index_file_size: u64,
  index_path: PathBuf,
  leaf_pages: u64,
  metadata_bytes: u64,
  outputs_traversed: u64,
  page_size: usize,
  sat_ranges: u64,
  stored_bytes: u64,
  tables: BTreeMap<String, TableInfo>,
  total_bytes: u64,
  pub transactions: Vec<TransactionInfo>,
  tree_height: u32,
  utxos_indexed: u64,
}

#[derive(Serialize)]
pub(crate) struct TableInfo {
  branch_pages: u64,
  fragmented_bytes: u64,
  leaf_pages: u64,
  metadata_bytes: u64,
  proportion: f64,
  stored_bytes: u64,
  total_bytes: u64,
  tree_height: u32,
}

#[derive(Serialize)]
pub struct TransactionInfo {
  pub starting_block_count: u32,
  pub starting_timestamp: u128,
}

pub(crate) trait BitcoinCoreRpcResultExt<T> {
  fn into_option(self) -> Result<Option<T>>;
}

impl<T> BitcoinCoreRpcResultExt<T> for Result<T, bitcoincore_rpc::Error> {
  fn into_option(self) -> Result<Option<T>> {
    match self {
      Ok(ok) => Ok(Some(ok)),
      Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::error::Error::Rpc(
        bitcoincore_rpc::jsonrpc::error::RpcError { code: -8, .. },
      ))) => Ok(None),
      Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::error::Error::Rpc(
        bitcoincore_rpc::jsonrpc::error::RpcError {
          code: -5, message, ..
        },
      )))
        if message.starts_with("No such mempool or blockchain transaction") =>
      {
        Ok(None)
      }
      Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::error::Error::Rpc(
        bitcoincore_rpc::jsonrpc::error::RpcError { message, .. },
      )))
        if message.ends_with("not found") =>
      {
        Ok(None)
      }
      Err(err) => Err(err.into()),
    }
  }
}

pub struct Index {
  pub(crate) client: Client,
  height_to_block_header: DB,
  height_to_last_sequence_number: DB,
  outpoint_to_utxo_entry: DB,
  inscription_id_to_sequence_number: DB,
  inscription_number_to_sequence_number: DB,
  inscription_id_to_txcnt: DB,
  sequence_number_to_inscription_entry: DB,
  statistic_to_count: DB,
  ord_transfers: DB,
  ord_inscription_info: DB,
  ord_index_stats: DB,
  event_sender: Option<tokio::sync::mpsc::Sender<Event>>,
  height_limit: Option<u32>,
  settings: Settings,
  first_index_height: u32,
  unrecoverably_reorged: AtomicBool,
  pub(crate) path: PathBuf,
}

impl Index {
  pub fn open(settings: &Settings) -> Result<Self> {
    Index::open_with_event_sender(settings, None)
  }

  pub fn open_with_event_sender(
    settings: &Settings,
    event_sender: Option<tokio::sync::mpsc::Sender<Event>>,
  ) -> Result<Self> {
    let client = settings.bitcoin_rpc_client(None)?;

    let path = settings.index().to_owned();

    let data_dir = path.parent().unwrap();

    fs::create_dir_all(data_dir).snafu_context(error::Io { path: data_dir })?;

    let index_cache_size = settings.index_cache_size();

    log::info!("Setting index cache size to {} bytes", index_cache_size);

    let index_path = path.clone();

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_max_open_files(256);

    rlimit::Resource::NOFILE.set(4096, 8192)?;

    let height_to_block_header_path = index_path.join("height_to_block_header.db");
    let height_to_block_header = DB::open(&opts, &height_to_block_header_path)?;

    let outpoint_to_utxo_entry_path = index_path.join("outpoint_to_utxo_entry.db");
    let outpoint_to_utxo_entry = DB::open(&opts, &outpoint_to_utxo_entry_path)?;

    let inscription_id_to_sequence_number_path = index_path.join("inscription_id_to_sequence_number.db");
    let inscription_id_to_sequence_number = DB::open(&opts, &inscription_id_to_sequence_number_path)?;

    let inscription_number_to_sequence_number_path = index_path.join("inscription_number_to_sequence_number.db");
    let inscription_number_to_sequence_number = DB::open(&opts, &inscription_number_to_sequence_number_path)?;

    let inscription_id_to_txcnt_path = index_path.join("inscription_id_to_txcnt.db");
    let inscription_id_to_txcnt = DB::open(&opts, &inscription_id_to_txcnt_path)?;

    let sequence_number_to_inscription_entry_path = index_path.join("sequence_number_to_inscription_entry.db");
    let sequence_number_to_inscription_entry = DB::open(&opts, &sequence_number_to_inscription_entry_path)?;

    let height_to_last_sequence_number_path = index_path.join("height_to_last_sequence_number.db");
    let height_to_last_sequence_number = DB::open(&opts, &height_to_last_sequence_number_path)?;

    let statistic_to_count_path = index_path.join("statistic_to_count.db");
    let statistic_to_count = DB::open(&opts, &statistic_to_count_path)?;

    let ord_transfers_path = index_path.join("ord_transfers.db");
    let ord_transfers = DB::open(&opts, &ord_transfers_path)?;

    let ord_inscription_info_path = index_path.join("ord_inscription_info.db");
    let ord_inscription_info = DB::open(&opts, &ord_inscription_info_path)?;

    let ord_index_stats_path = index_path.join("ord_index_stats.db");
    let ord_index_stats = DB::open(&opts, &ord_index_stats_path)?;

    let schema_version = statistic_to_count.get(&Statistic::Schema.key().to_be_bytes());
    if schema_version.is_err() || schema_version.as_ref().unwrap().is_none() {
      println!(
        "Initializing index schema version {} at {}",
        SCHEMA_VERSION,
        index_path.display()
      );

      // If the schema version is not set, we need to initialize it.
      statistic_to_count.put(&Statistic::Schema.key().to_be_bytes(), &SCHEMA_VERSION.to_be_bytes())?;
    } else {
      let schema_version = u64::from_be_bytes(schema_version.unwrap().unwrap().try_into().unwrap());
      println!(
        "Index schema version {} at {}",
        schema_version,
        index_path.display()
      );

      if schema_version != SCHEMA_VERSION {
        bail!(
          "Incompatible index schema version: expected {}, found {}",
          SCHEMA_VERSION,
          schema_version
        );
      }
    }

    let first_index_height = settings.first_inscription_height();

    Ok(Self {
      client,
      height_to_block_header,
      height_to_last_sequence_number,
      outpoint_to_utxo_entry,
      inscription_id_to_sequence_number,
      inscription_number_to_sequence_number,
      inscription_id_to_txcnt,
      sequence_number_to_inscription_entry,
      statistic_to_count,
      ord_transfers,
      ord_inscription_info,
      ord_index_stats,
      event_sender,
      first_index_height,
      height_limit: settings.height_limit(),
      settings: settings.clone(),
      unrecoverably_reorged: AtomicBool::new(false),
      path,
    })
  }

  pub fn have_full_utxo_index(&self) -> bool {
    self.first_index_height == 0
  }

  /// Unlike normal outpoints, which are added to index on creation and removed
  /// when spent, the UTXO entry for special outpoints may be updated.
  ///
  /// The special outpoints are the null outpoint, which receives lost sats,
  /// and the unbound outpoint, which receives unbound inscriptions.
  pub fn is_special_outpoint(outpoint: OutPoint) -> bool {
    outpoint == OutPoint::null() || outpoint == unbound_outpoint()
  }

  pub fn update(&self) -> Result {
    loop {
      let blocks_indexed = self.height_to_block_header.iterator(IteratorMode::End)
        .next()
        .transpose()?
        .map(|(height, _header)| u32::from_be_bytes((*height).try_into().unwrap()) + 1)
        .unwrap_or(0);
      
      let mut updater = Updater {
        height: blocks_indexed,
        index: self,
        outputs_cached: 0,
        outputs_traversed: 0,
        sat_ranges_since_flush: 0,
      };

      match updater.update_index() {
        Ok(ok) => return Ok(ok),
        Err(err) => {
          log::info!("{err}");

          match err.downcast_ref() {
            Some(&reorg::Error::Unrecoverable) => {
              self
                .unrecoverably_reorged
                .store(true, atomic::Ordering::Relaxed);
              return Err(anyhow!(reorg::Error::Unrecoverable));
            }
            _ => return Err(err),
          };
        }
      }
    }
  }

  pub fn block_count(&self) -> Result<u32> {
    let blocks_indexed = self.height_to_block_header.iterator(IteratorMode::End)
      .next()
      .transpose()?
      .map(|(height, _header)| u32::from_be_bytes((*height).try_into().unwrap()) + 1)
      .unwrap_or(0);

    Ok(blocks_indexed)
  }

  pub fn block_height(&self) -> Result<Option<Height>> {
    let block_height = self.height_to_block_header.iterator(IteratorMode::End)
      .next()
      .transpose()?
      .map(|(height, _header)| Height(u32::from_be_bytes((*height).try_into().unwrap())));

    Ok(block_height)
  }

  pub fn block_hash(&self, height: Option<u32>) -> Result<Option<BlockHash>> {
    Ok(
      match height {
        Some(height) => self.height_to_block_header.get(&height.to_be_bytes()).unwrap(),
        None => self.height_to_block_header.iterator(IteratorMode::End)
          .next()
          .transpose()?
          .map(|(_height, header)| (*header).to_vec()),
      }
      .map(|header| Header::load(header.try_into().unwrap()).block_hash()),
    )
  }

  pub fn blocks(&self, take: usize) -> Result<Vec<(u32, BlockHash)>> {
    let mut blocks = Vec::with_capacity(take);

    for next in self.height_to_block_header.iterator(IteratorMode::End)
      .take(take)
    {
      let next = next?;
      blocks.push((u32::from_be_bytes((*next.0).try_into().unwrap()), Header::load((*next.1).try_into().unwrap()).block_hash()));
    }

    Ok(blocks)
  }

  pub fn get_inscription_entry(
    &self,
    inscription_id: InscriptionId,
  ) -> Result<Option<InscriptionEntry>> {
    let Some(sequence_number) = self
      .inscription_id_to_sequence_number
      .get(&inscription_id.store())?
      .map(|value| u32::from_be_bytes(value.try_into().unwrap()))
    else {
      return Ok(None);
    };

    let entry = self
      .sequence_number_to_inscription_entry
      .get(&sequence_number.to_be_bytes())?
      .map(|value| InscriptionEntry::load(value.try_into().unwrap()));
    Ok(entry)
  }
}
