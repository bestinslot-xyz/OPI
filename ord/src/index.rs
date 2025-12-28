use {
  self::{
    entry::{Entry, InscriptionEntry, SatRange},
    event::Event,
    reorg::Reorg,
    updater::Updater,
    utxo_entry::{ParsedUtxoEntry, UtxoEntryBuf},
  },
  super::*,
  bitcoin::block::Header,
  bitcoincore_rpc::Client,
  db_reader::{start_rpc_server, Config},
  indicatif::{ProgressBar, ProgressStyle},
  log::log_enabled,
  rocksdb::{
    backup::{BackupEngine, BackupEngineOptions},
    ColumnFamilyDescriptor, IteratorMode, Options, DB,
  },
  std::collections::HashMap,
  tokio::runtime::Runtime,
};

pub use updater::get_tx_limits;

pub(crate) mod entry;
pub mod event;
mod fetcher;
mod lot;
pub(crate) mod reorg;
mod updater;
mod utxo_entry;

const SCHEMA_VERSION: u64 = 99100030;

#[derive(Copy, Clone)]
pub(crate) enum Statistic {
  Schema = 0,
  BlessedInscriptions = 1,
  CursedInscriptions = 3,
  LastSavepointHeight = 17,
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
  db: DB,
  event_sender: Option<tokio::sync::mpsc::Sender<Event>>,
  height_limit: Option<u32>,
  settings: Settings,
  first_index_height: u32,
  unrecoverably_reorged: AtomicBool,
  write_options: rocksdb::WriteOptions,
  pub(crate) path: PathBuf,
  _runtime: Runtime,
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
    opts.set_write_buffer_size(8192 * 1024 * 1024); // 8 GiB
    opts.set_atomic_flush(true);
    opts.create_missing_column_families(true);
    opts.enable_statistics();

    let mut cf_opts = Options::default();
    cf_opts.set_write_buffer_size(8192 * 1024 * 1024); // 8 GiB

    let write_options = {
      let mut write_options = rocksdb::WriteOptions::default();
      write_options.disable_wal(true);
      write_options
    };

    rlimit::Resource::NOFILE.set(
      option_env!("NOFILE_SOFT_LIMIT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(65536),
      option_env!("NOFILE_HARD_LIMIT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(131072),
    )?;

    let column_families = vec![
      ColumnFamilyDescriptor::new("height_to_block_header", cf_opts.clone()),
      ColumnFamilyDescriptor::new("height_to_last_sequence_number", cf_opts.clone()),
      ColumnFamilyDescriptor::new("outpoint_to_utxo_entry", cf_opts.clone()),
      ColumnFamilyDescriptor::new("inscription_id_to_sequence_number", cf_opts.clone()),
      ColumnFamilyDescriptor::new("inscription_number_to_sequence_number", cf_opts.clone()),
      ColumnFamilyDescriptor::new("inscription_id_to_txcnt", cf_opts.clone()),
      ColumnFamilyDescriptor::new("sequence_number_to_inscription_entry", cf_opts.clone()),
      ColumnFamilyDescriptor::new("statistic_to_count", cf_opts.clone()),
      ColumnFamilyDescriptor::new("ord_transfers", cf_opts.clone()),
      ColumnFamilyDescriptor::new("ord_inscription_info", cf_opts.clone()),
      ColumnFamilyDescriptor::new("ord_index_stats", cf_opts.clone()),
    ];

    let db_path = path.join("index.db");
    let db = DB::open_cf_descriptors(&opts, &db_path, column_families)?;
    let statistic_to_count = db
      .cf_handle("statistic_to_count")
      .ok_or_else(|| anyhow!("Failed to open column family 'statistic_to_count'"))?;

    let schema_version = db.get_cf(statistic_to_count, &Statistic::Schema.key().to_be_bytes());
    if schema_version.is_err() || schema_version.as_ref().unwrap().is_none() {
      println!(
        "Initializing index schema version {} at {}",
        SCHEMA_VERSION,
        index_path.display()
      );

      // If the schema version is not set, we need to initialize it.
      db.put_cf_opt(
        statistic_to_count,
        &Statistic::Schema.key().to_be_bytes(),
        &SCHEMA_VERSION.to_be_bytes(),
        &write_options,
      )?;
      db.flush()?;
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

    let chain = settings.chain();
    let db_path = path.clone();
    let runtime = Runtime::new()?;
    runtime.spawn(async move {
      println!("Starting RPC server for index at {}", db_path.display());
      start_rpc_server(Config {
        network: match chain {
          Chain::Mainnet => bitcoin::Network::Bitcoin,
          Chain::Testnet => bitcoin::Network::Testnet,
          Chain::Testnet4 => bitcoin::Network::Testnet4,
          Chain::Signet => bitcoin::Network::Signet,
          Chain::Regtest => bitcoin::Network::Regtest,
        },
        db_path: Some(db_path.canonicalize().unwrap()),
        api_url: std::env::var("DB_READER_API_URL").ok(),
      })
      .await
      .unwrap()
    });

    Ok(Self {
      client,
      db,
      event_sender,
      first_index_height,
      height_limit: settings.height_limit(),
      settings: settings.clone(),
      unrecoverably_reorged: AtomicBool::new(false),
      path,
      write_options,
      _runtime: runtime,
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
      if SHUTTING_DOWN.load(atomic::Ordering::Relaxed) {
        return Ok(());
      }

      let height_to_block_header = self
        .db
        .cf_handle("height_to_block_header")
        .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;

      let blocks_indexed = self
        .db
        .iterator_cf(height_to_block_header, IteratorMode::End)
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
        Ok(_ok) => {
          thread::sleep(Duration::from_secs(5));
        }
        Err(err) => {
          log::info!("{err}");

          match err.downcast_ref() {
            Some(&reorg::Error::Recoverable {
              height: _,
              depth: _,
            }) => {
              return Err(err); // Reorg::handle_reorg(self, height, depth)?;
            }
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
    let height_to_block_header = self
      .db
      .cf_handle("height_to_block_header")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;

    let blocks_indexed = self
      .db
      .iterator_cf(height_to_block_header, IteratorMode::End)
      .next()
      .transpose()?
      .map(|(height, _header)| u32::from_be_bytes((*height).try_into().unwrap()) + 1)
      .unwrap_or(0);

    Ok(blocks_indexed)
  }

  pub fn block_height(&self) -> Result<Option<Height>> {
    let height_to_block_header = self
      .db
      .cf_handle("height_to_block_header")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;

    let block_height = self
      .db
      .iterator_cf(height_to_block_header, IteratorMode::End)
      .next()
      .transpose()?
      .map(|(height, _header)| Height(u32::from_be_bytes((*height).try_into().unwrap())));

    Ok(block_height)
  }

  pub fn block_hash(&self, height: Option<u32>) -> Result<Option<BlockHash>> {
    let height_to_block_header = self
      .db
      .cf_handle("height_to_block_header")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;

    Ok(
      match height {
        Some(height) => self
          .db
          .get_cf(height_to_block_header, &height.to_be_bytes())
          .unwrap(),
        None => self
          .db
          .iterator_cf(height_to_block_header, IteratorMode::End)
          .next()
          .transpose()?
          .map(|(_height, header)| (*header).to_vec()),
      }
      .map(|header| Header::load(header.try_into().unwrap()).block_hash()),
    )
  }

  pub fn blocks(&self, take: usize) -> Result<Vec<(u32, BlockHash)>> {
    let height_to_block_header = self
      .db
      .cf_handle("height_to_block_header")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;

    let mut blocks = Vec::with_capacity(take);

    for next in self
      .db
      .iterator_cf(height_to_block_header, IteratorMode::End)
      .take(take)
    {
      let next = next?;
      blocks.push((
        u32::from_be_bytes((*next.0).try_into().unwrap()),
        Header::load((*next.1).try_into().unwrap()).block_hash(),
      ));
    }

    Ok(blocks)
  }

  pub fn get_inscription_entry(
    &self,
    inscription_id: InscriptionId,
  ) -> Result<Option<InscriptionEntry>> {
    let inscription_id_to_sequence_number = self
      .db
      .cf_handle("inscription_id_to_sequence_number")
      .ok_or_else(|| anyhow!("Failed to open column family 'inscription_id_to_sequence_number'"))?;

    let Some(sequence_number) = self
      .db
      .get_cf(inscription_id_to_sequence_number, &inscription_id.store())?
      .map(|value| u32::from_be_bytes(value.try_into().unwrap()))
    else {
      return Ok(None);
    };

    let sequence_number_to_inscription_entry = self
      .db
      .cf_handle("sequence_number_to_inscription_entry")
      .ok_or_else(|| {
        anyhow!("Failed to open column family 'sequence_number_to_inscription_entry'")
      })?;

    let entry = self
      .db
      .get_cf(
        sequence_number_to_inscription_entry,
        &sequence_number.to_be_bytes(),
      )?
      .map(|value| InscriptionEntry::load(value.try_into().unwrap()));
    Ok(entry)
  }
}
