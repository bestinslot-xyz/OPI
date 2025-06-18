use {
  self::inscription_updater::{InscriptionUpdater, TX_LIMITS}, super::{fetcher::Fetcher, *}, futures::future::try_join_all, rocksdb::ColumnFamily, tokio::sync::{
    broadcast::{self, error::TryRecvError},
    mpsc::{self},
  }
};

mod inscription_updater;

pub fn get_tx_limits() -> HashMap<String, i16> {
  let mut tx_limits = HashMap::new();
  for (key, value) in TX_LIMITS.iter() {
    tx_limits.insert(key.to_string(), *value);
  }
  tx_limits
}

pub(crate) struct BlockData {
  pub(crate) header: Header,
  pub(crate) txdata: Vec<(Transaction, Txid)>,
}

impl From<Block> for BlockData {
  fn from(block: Block) -> Self {
    BlockData {
      header: block.header,
      txdata: block
        .txdata
        .into_iter()
        .map(|transaction| {
          let txid = transaction.compute_txid();
          (transaction, txid)
        })
        .collect(),
    }
  }
}

pub(crate) struct Updater<'index> {
  pub(super) height: u32,
  pub(super) index: &'index Index,
  pub(super) outputs_cached: u64,
  pub(super) outputs_traversed: u64,
  pub(super) sat_ranges_since_flush: u64,
}

impl Updater<'_> {
  pub(crate) fn update_index(&mut self) -> Result {
    let starting_height = u32::try_from(self.index.client.get_block_count()?).unwrap() + 1;

    let mut progress_bar = if cfg!(test)
      || log_enabled!(log::Level::Info)
      || starting_height <= self.height
      || self.index.settings.integration_test()
    {
      None
    } else {
      let progress_bar = ProgressBar::new(starting_height.into());
      progress_bar.set_position(self.height.into());
      progress_bar.set_style(
        ProgressStyle::with_template("[indexing blocks] {wide_bar} {pos}/{len}").unwrap(),
      );
      Some(progress_bar)
    };

    let rx = Self::fetch_blocks_from(self.index, self.height)?;

    let (mut output_sender, mut txout_receiver) = Self::spawn_fetcher(self.index)?;

    println!(
      "Indexing blocks from height {} to {}…",
      self.height,
      starting_height
    );

    let mut uncommitted = 0;
    let mut utxo_cache = HashMap::new();
    let mut tm = Instant::now();
    let mut last_stat_print_height = self.height;
    let mut gtms = [0; 3];
    let ord_index_stats = self.index.db.cf_handle("ord_index_stats")
      .ok_or_else(|| anyhow!("Failed to open column family 'ord_index_stats'"))?;
    let mut last_flush_bytes: u64 = 0;
    while let Ok(block) = rx.recv() {
      let mut tms = [0; 3];
      tms[0] = tm.elapsed().as_millis();
      gtms[0] += tms[0];
      tm = Instant::now();

      self.index.db.property_value("rocksdb.cur-size-all-mem-tables")
        .map(|size| {
          if size.is_none() {
            println!("RocksDB memtable size is not available");
            return;
          }

          if let Ok(size) = size.unwrap().parse::<u64>() {
            if size > 1024 * 1024 {
              println!("RocksDB memtable size is too large: {size} bytes");
            } else {
              log::debug!("RocksDB memtable size: {size} bytes");
            }
          }
        })
        .unwrap_or_else(|err| log::error!("Failed to get RocksDB memtable size: {err}"));


      self.index.db.property_value("rocksdb.options-statistics")
        .map(|stats| {
          if stats.is_none() {
            println!("RocksDB options-statistics is not available");
            return;
          }

          let stats_unw = stats.unwrap();
          // find the line starting with "rocksdb.flush.write.bytes"
          if let Some(line) = stats_unw.lines().find(|line| line.starts_with("rocksdb.flush.write.bytes")) {
            // split line from : and parse the right part as u64
            if let Some(value) = line.split_once(": ") {
              if let Ok(value) = value.1.trim().parse::<u64>() {
                if value != last_flush_bytes {
                  let diff = value.saturating_sub(last_flush_bytes);
                  let diff_mb = diff as f64 / (1024.0 * 1024.0);
                  println!(
                    "RocksDB incr. flush write: {diff_mb:.3} MB"
                  );
                  last_flush_bytes = value;
                }
              }
            }
          }
          if let Some(line) = stats_unw.lines().find(|line| line.starts_with("rocksdb.wal.bytes")) {
            // split line from : and parse the right part as u64
            if let Some(value) = line.split_once(": ") {
              if let Ok(value) = value.1.trim().parse::<u64>() {
                if value != 0 {
                  let value_kb = value as f64 / (1024.0);
                  println!("RocksDB total WAL size: {value_kb:.3} KB");
                }
              }
            }
          }
        })
        .unwrap_or_else(|err| println!("Failed to get RocksDB options-statistics: {err}"));

      self.index_block(
        &mut output_sender,
        &mut txout_receiver,
        block,
        &mut utxo_cache,
      )?;

      tms[1] = tm.elapsed().as_millis();
      gtms[1] += tms[1];
      tm = Instant::now();

      if let Some(progress_bar) = &mut progress_bar {
        progress_bar.inc(1);

        if progress_bar.position() > progress_bar.length().unwrap() {
          if let Ok(count) = self.index.client.get_block_count() {
            progress_bar.set_length(count + 1);
          } else {
            log::warn!("Failed to fetch latest block height");
          }
        }
      }

      uncommitted += 1;

      if uncommitted == self.index.settings.commit_interval()
        || (!self.index.settings.integration_test()
          && Reorg::is_savepoint_required(self.index, self.height)?)
      {
        self.commit(utxo_cache)?;
        utxo_cache = HashMap::new();
        uncommitted = 0;

        let height = self.index.block_count()?;
        if height != self.height {
          println!(
            "Height changed from {} to {}!!!",
            self.height, height
          );
          // another update has run between committing and beginning the new
          // write transaction
          break;
        }
      }

      if SHUTTING_DOWN.load(atomic::Ordering::Relaxed) {
        break;
      }

      tms[2] = tm.elapsed().as_millis();
      gtms[2] += tms[2];
      tm = Instant::now();

      let ord_index_stat_key = self.height.to_be_bytes();
      let ord_index_stat_data = [
        tms[0].to_be_bytes(),
        tms[1].to_be_bytes(),
        tms[2].to_be_bytes(),
        (tms[0] + tms[1] + tms[2]).to_be_bytes(),
      ].concat();
      self.index.db.put_cf_opt(
        ord_index_stats,
        &ord_index_stat_key,
        &ord_index_stat_data,
        &self.index.write_options,
      )?;

      if self.height % 500 == 430 {
        println!(
          "Height {}: {} ms for fetch, {} ms for index, {} ms for savepoint/commit, {} ms total",
          self.height,
          gtms[0],
          gtms[1],
          gtms[2],
          (gtms[0] + gtms[1] + gtms[2]),
        );
        println!(
          "Height {}: {} ms for fetch per block, {} ms for index per block, {} ms for savepoint/commit per block, {} ms total per block",
          self.height,
          gtms[0] / ((self.height - last_stat_print_height) as u128),
          gtms[1] / ((self.height - last_stat_print_height) as u128),
          gtms[2] / ((self.height - last_stat_print_height) as u128),
          (gtms[0] + gtms[1] + gtms[2]) / ((self.height - last_stat_print_height) as u128),
        );
        last_stat_print_height = self.height;
        gtms = [0; 3];
      }
    }

    if uncommitted > 0 {
      self.commit(utxo_cache)?;
    }

    if let Some(progress_bar) = &mut progress_bar {
      progress_bar.finish_and_clear();
    }

    self.index.db.property_value("rocksdb.options-statistics")
      .map(|stats| {
        if stats.is_none() {
          println!("RocksDB options-statistics is not available");
          return;
        }

        let stats_unw = stats.unwrap();
        // find the line starting with "rocksdb.flush.write.bytes"
        if let Some(line) = stats_unw.lines().find(|line| line.starts_with("rocksdb.flush.write.bytes")) {
          // split line from : and parse the right part as u64
          if let Some(value) = line.split_once(": ") {
            if let Ok(value) = value.1.trim().parse::<u64>() {
              let diff = value.saturating_sub(last_flush_bytes);
              let diff_mb = diff as f64 / (1024.0 * 1024.0);
              println!(
                "RocksDB incr. flush write: {diff_mb:.3} MB"
              );
              let value_mb = value as f64 / (1024.0 * 1024.0);
              println!("RocksDB total flush write: {value_mb:.3} MB");
            }
          }
        }
      })
      .unwrap_or_else(|err| println!("Failed to get RocksDB options-statistics: {err}"));

    Ok(())
  }

  fn fetch_blocks_from(
    index: &Index,
    mut height: u32,
  ) -> Result<std::sync::mpsc::Receiver<BlockData>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(32);

    let first_index_height = index.first_index_height;

    let height_limit = index.height_limit;

    let client = index.settings.bitcoin_rpc_client(None)?;

    thread::spawn(move || loop {
      if let Some(height_limit) = height_limit {
        if height >= height_limit {
          break;
        }
      }

      match Self::get_block_with_retries(&client, height, first_index_height) {
        Ok(Some(block)) => {
          if let Err(err) = tx.send(block.into()) {
            log::info!("Block receiver disconnected: {err}");
            break;
          }
          height += 1;
        }
        Ok(None) => break,
        Err(err) => {
          log::error!("failed to fetch block {height}: {err}");
          break;
        }
      }
    });

    Ok(rx)
  }

  fn get_block_with_retries(
    client: &Client,
    height: u32,
    first_index_height: u32,
  ) -> Result<Option<Block>> {
    let mut errors = 0;
    loop {
      match client
        .get_block_hash(height.into())
        .into_option()
        .and_then(|option| {
          option
            .map(|hash| {
              if height >= first_index_height {
                Ok(client.get_block(&hash)?)
              } else {
                Ok(Block {
                  header: client.get_block_header(&hash)?,
                  txdata: Vec::new(),
                })
              }
            })
            .transpose()
        }) {
        Err(err) => {
          if cfg!(test) {
            return Err(err);
          }

          errors += 1;
          let seconds = 1 << errors;
          log::warn!("failed to fetch block {height}, retrying in {seconds}s: {err}");

          if seconds > 120 {
            log::error!("would sleep for more than 120s, giving up");
            return Err(err);
          }

          thread::sleep(Duration::from_secs(seconds));
        }
        Ok(result) => return Ok(result),
      }
    }
  }

  fn spawn_fetcher(index: &Index) -> Result<(mpsc::Sender<OutPoint>, broadcast::Receiver<TxOut>)> {
    let fetcher = Fetcher::new(&index.settings)?;

    // A block probably has no more than 20k inputs
    const CHANNEL_BUFFER_SIZE: usize = 20_000;

    // Batch 2048 missing inputs at a time, arbitrarily chosen size
    const BATCH_SIZE: usize = 2048;

    let (outpoint_sender, mut outpoint_receiver) = mpsc::channel::<OutPoint>(CHANNEL_BUFFER_SIZE);

    let (txout_sender, txout_receiver) = broadcast::channel::<TxOut>(CHANNEL_BUFFER_SIZE);

    // Default rpcworkqueue in bitcoind is 16, meaning more than 16 concurrent requests will be rejected.
    // Since we are already requesting blocks on a separate thread, and we don't want to break if anything
    // else runs a request, we keep this to 12.
    let parallel_requests: usize = index.settings.bitcoin_rpc_limit().try_into().unwrap();

    thread::spawn(move || {
      let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
      rt.block_on(async move {
        loop {
          let Some(outpoint) = outpoint_receiver.recv().await else {
            log::debug!("Outpoint channel closed");
            return;
          };

          // There's no try_iter on tokio::sync::mpsc::Receiver like std::sync::mpsc::Receiver.
          // So we just loop until BATCH_SIZE doing try_recv until it returns None.
          let mut outpoints = vec![outpoint];
          for _ in 0..BATCH_SIZE - 1 {
            let Ok(outpoint) = outpoint_receiver.try_recv() else {
              break;
            };
            outpoints.push(outpoint);
          }

          // Break outputs into chunks for parallel requests
          let chunk_size = (outpoints.len() / parallel_requests) + 1;
          let mut futs = Vec::with_capacity(parallel_requests);
          for chunk in outpoints.chunks(chunk_size) {
            let txids = chunk.iter().map(|outpoint| outpoint.txid).collect();
            let fut = fetcher.get_transactions(txids);
            futs.push(fut);
          }

          let txs = match try_join_all(futs).await {
            Ok(txs) => txs,
            Err(e) => {
              log::error!("Couldn't receive txs {e}");
              return;
            }
          };

          // Send all tx outputs back in order
          for (i, tx) in txs.iter().flatten().enumerate() {
            let Ok(_) =
              txout_sender.send(tx.output[usize::try_from(outpoints[i].vout).unwrap()].clone())
            else {
              log::error!("Value channel closed unexpectedly");
              return;
            };
          }
        }
      })
    });

    Ok((outpoint_sender, txout_receiver))
  }

  fn index_block(
    &mut self,
    output_sender: &mut mpsc::Sender<OutPoint>,
    txout_receiver: &mut broadcast::Receiver<TxOut>,
    block: BlockData,
    utxo_cache: &mut HashMap<OutPoint, UtxoEntryBuf>,
  ) -> Result<()> {
    /*lazy_static! {
      static ref LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
    }
    let mut log_file = LOG_FILE.lock().unwrap();
    if log_file.as_ref().is_none() {
      let chain_folder: String = match self.index.settings.chain() {
        Chain::Mainnet => String::from(""),
        Chain::Testnet => String::from("testnet3/"),
        Chain::Testnet4 => String::from("testnet4/"),
        Chain::Signet => String::from("signet/"),
        Chain::Regtest => String::from("regtest/"),
      };
      *log_file = Some(File::options().append(true).open(format!("{chain_folder}log_file_index.txt")).unwrap());
    }
    println!("cmd;{0};new_block;{1};{2}", self.height, &block.header.block_hash(), block.header.time);
    writeln!(log_file.as_ref().unwrap(), "cmd;{0};new_block;{1};{2}", self.height, &block.header.block_hash(), block.header.time)?;
    (log_file.as_ref().unwrap()).flush()?;*/

    Reorg::detect_reorg(&block, self.height, self.index)?;

    let start = Instant::now();
    let mut sat_ranges_written = 0;
    let mut outputs_in_block = 0;

    log::info!(
      "Block {} at {} with {} transactions…",
      self.height,
      timestamp(block.header.time.into()),
      block.txdata.len()
    );

    let height_to_block_header = self.index.db.cf_handle("height_to_block_header")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?;
    let inscription_id_to_sequence_number = self.index.db.cf_handle("inscription_id_to_sequence_number")
      .ok_or_else(|| anyhow!("Failed to open column family 'inscription_id_to_sequence_number'"))?;
    let statistic_to_count = self.index.db.cf_handle("statistic_to_count")
      .ok_or_else(|| anyhow!("Failed to open column family 'statistic_to_count'"))?;

    self.index_utxo_entries(
      &block,
      txout_receiver,
      output_sender,
      utxo_cache,
      inscription_id_to_sequence_number,
      statistic_to_count,
      &mut sat_ranges_written,
      &mut outputs_in_block,
    )?;

    self.index.db.put_cf_opt(
      height_to_block_header,
      &self.height.to_be_bytes(),
      &block.header.store(),
      &self.index.write_options,
    )?;

    self.height += 1;
    self.outputs_traversed += outputs_in_block;

    log::info!(
      "Wrote {sat_ranges_written} sat ranges from {outputs_in_block} outputs in {} ms",
      (Instant::now() - start).as_millis(),
    );

    Ok(())
  }

  /* else if let Some(entry) = outpoint_to_utxo_entry.remove(&outpoint)? {
              entry.value().to_buf()
            } */
  fn get_and_remove_if_exists(
    &mut self,
    column_family: &ColumnFamily,
    key: &[u8],
  ) -> Option<UtxoEntryBuf> {
    let res = self.index.db.get_cf(column_family, key).unwrap();

    if res.is_none() {
      return None;
    }

    self.index.db.delete_cf_opt(
      column_family,
      key,
      &self.index.write_options,
    ).unwrap();
    Some(UtxoEntryBuf::new_with_values(res.unwrap()))
  }

  fn index_utxo_entries<'wtx>(
    &mut self,
    block: &BlockData,
    txout_receiver: &mut broadcast::Receiver<TxOut>,
    output_sender: &mut mpsc::Sender<OutPoint>,
    utxo_cache: &mut HashMap<OutPoint, UtxoEntryBuf>,
    inscription_id_to_sequence_number: &ColumnFamily,
    statistic_to_count: &ColumnFamily,
    _sat_ranges_written: &mut u64,
    _outputs_in_block: &mut u64,
  ) -> Result<(), Error> {
    let height_to_last_sequence_number = self.index.db.cf_handle("height_to_last_sequence_number")
      .ok_or_else(|| anyhow!("Failed to open column family 'height_to_last_sequence_number'"))?;
    let inscription_number_to_sequence_number = self.index.db.cf_handle("inscription_number_to_sequence_number")
      .ok_or_else(|| anyhow!("Failed to open column family 'inscription_number_to_sequence_number'"))?;
    let outpoint_to_utxo_entry = self.index.db.cf_handle("outpoint_to_utxo_entry")
      .ok_or_else(|| anyhow!("Failed to open column family 'outpoint_to_utxo_entry'"))?;
    let inscription_id_to_txcnt = self.index.db.cf_handle("inscription_id_to_txcnt")
      .ok_or_else(|| anyhow!("Failed to open column family 'inscription_id_to_txcnt'"))?;
    let sequence_number_to_inscription_entry = self.index.db.cf_handle("sequence_number_to_inscription_entry")
      .ok_or_else(|| anyhow!("Failed to open column family 'sequence_number_to_inscription_entry'"))?;
    let ord_transfers = self.index.db.cf_handle("ord_transfers")
      .ok_or_else(|| anyhow!("Failed to open column family 'ord_transfers'"))?;
    let ord_inscription_info = self.index.db.cf_handle("ord_inscription_info")
      .ok_or_else(|| anyhow!("Failed to open column family 'ord_inscription_info'"))?;

    let index_inscriptions = self.height >= self.index.settings.first_inscription_height();

    // If the receiver still has inputs something went wrong in the last
    // block and we shouldn't recover from this and commit the last block
    if index_inscriptions {
      assert!(
        matches!(txout_receiver.try_recv(), Err(TryRecvError::Empty)),
        "Previous block did not consume all inputs"
      );
    }

    if !self.index.have_full_utxo_index() {
      // Send all missing input outpoints to be fetched
      let txids = block
        .txdata
        .iter()
        .map(|(_, txid)| txid)
        .collect::<HashSet<_>>();

      for (tx, _) in &block.txdata {
        for input in &tx.input {
          let prev_output = input.previous_output;
          // We don't need coinbase inputs
          if prev_output.is_null() {
            continue;
          }
          // We don't need inputs from txs earlier in the block, since
          // they'll be added to cache when the tx is indexed
          if txids.contains(&prev_output.txid) {
            continue;
          }
          // We don't need inputs we already have in our cache from earlier blocks
          if utxo_cache.contains_key(&prev_output) {
            continue;
          }
          // We don't need inputs we already have in our database
          if self.index.db.get_cf(outpoint_to_utxo_entry, &prev_output.store())?.is_some() {
            continue;
          }
          // Send this outpoint to background thread to be fetched
          output_sender.blocking_send(prev_output)?;
        }
      }
    }

    let cursed_inscription_count = self.index.db
      .get_cf(statistic_to_count, &Statistic::CursedInscriptions.key().to_be_bytes())?
      .map(|count| u64::from_be_bytes(count.try_into().unwrap()))
      .unwrap_or(0);

    let blessed_inscription_count = self.index.db
      .get_cf(statistic_to_count, &Statistic::BlessedInscriptions.key().to_be_bytes())?
      .map(|count| u64::from_be_bytes(count.try_into().unwrap()))
      .unwrap_or(0);

    let next_sequence_number = self.index.db
      .iterator_cf(sequence_number_to_inscription_entry, IteratorMode::End)
      .next()
      .transpose()?
      .map(|(number, _id)| u32::from_be_bytes((*number).try_into().unwrap()) + 1)
      .unwrap_or(0);

    let mut inscription_updater = InscriptionUpdater {
      blessed_inscription_count,
      cursed_inscription_count,
      flotsam: Vec::new(),
      height: self.height,
      db: &self.index.db,
      id_to_sequence_number: inscription_id_to_sequence_number,
      inscription_number_to_sequence_number: inscription_number_to_sequence_number,
      id_to_txcnt: inscription_id_to_txcnt,
      next_sequence_number,
      reward: Height(self.height).subsidy(),
      sequence_number_to_entry: sequence_number_to_inscription_entry,
      ord_transfers,
      ord_inscription_info,
      transfer_idx: 0,
      early_transfer_info: HashMap::new(),
      write_options: &self.index.write_options,
    };

    for (tx_offset, (tx, txid)) in block
      .txdata
      .iter()
      .enumerate()
      .skip(1)
      .chain(block.txdata.iter().enumerate().take(1))
    {
      log::trace!("Indexing transaction {tx_offset}…");

      let input_utxo_entries = if tx_offset == 0 {
        Vec::new()
      } else {
        tx.input
          .iter()
          .map(|input| {
            let outpoint = input.previous_output.store();

            let entry = if let Some(entry) = utxo_cache.remove(&OutPoint::load(outpoint)) {
              self.outputs_cached += 1;
              entry
            } else if let Some(entry) = self.get_and_remove_if_exists(outpoint_to_utxo_entry, &outpoint) {
              entry
            } else {
              assert!(!self.index.have_full_utxo_index());
              let txout = txout_receiver.blocking_recv().map_err(|err| {
                anyhow!(
                  "failed to get transaction for {}: {err}",
                  input.previous_output
                )
              })?;

              let mut entry = UtxoEntryBuf::new();
              entry.push_value(txout.value.to_sat());

              entry
            };

            Ok(entry)
          })
          .collect::<Result<Vec<UtxoEntryBuf>>>()?
      };

      let input_utxo_entries = input_utxo_entries
        .iter()
        .map(|entry| entry.parse())
        .collect::<Vec<ParsedUtxoEntry>>();

      let mut output_utxo_entries = tx
        .output
        .iter()
        .map(|_| UtxoEntryBuf::new())
        .collect::<Vec<UtxoEntryBuf>>();

      let input_sat_ranges;
      input_sat_ranges = None;

      for (vout, txout) in tx.output.iter().enumerate() {
        output_utxo_entries[vout].push_value(txout.value.to_sat());
      }

      if index_inscriptions {
        inscription_updater.index_inscriptions(
          tx,
          *txid,
          &input_utxo_entries,
          &mut output_utxo_entries,
          utxo_cache,
          self.index,
          input_sat_ranges.as_ref(),
        )?;
      }

      for (vout, output_utxo_entry) in output_utxo_entries.into_iter().enumerate() {
        let vout = u32::try_from(vout).unwrap();
        utxo_cache.insert(OutPoint { txid: *txid, vout }, output_utxo_entry);
      }
    }

    if index_inscriptions {
      inscription_updater.end_block()?;
      self.index.db
        .put_cf_opt(
          height_to_last_sequence_number,
          &self.height.to_be_bytes(),
          inscription_updater.next_sequence_number.to_be_bytes(),
          &self.index.write_options
        )?;
    }

    self.index.db.put_cf_opt(
      statistic_to_count,
      &Statistic::CursedInscriptions.key().to_be_bytes(),
      &inscription_updater.cursed_inscription_count.to_be_bytes(),
      &self.index.write_options,
    )?;

    self.index.db.put_cf_opt(
      statistic_to_count,
      &Statistic::BlessedInscriptions.key().to_be_bytes(),
      &inscription_updater.blessed_inscription_count.to_be_bytes(),
      &self.index.write_options,
    )?;

    Ok(())
  }

  fn commit(
    &mut self,
    utxo_cache: HashMap<OutPoint, UtxoEntryBuf>,
  ) -> Result {
    log::info!(
      "Committing at block height {}, {} outputs traversed, {} in map, {} cached",
      self.height,
      self.outputs_traversed,
      utxo_cache.len(),
      self.outputs_cached
    );

    let st_tm = Instant::now();
    println!(
      "Committing at block height {}, {} outputs traversed, {} in map, {} cached",
      self.height,
      self.outputs_traversed,
      utxo_cache.len(),
      self.outputs_cached
    );

    {
      let outpoint_to_utxo_entry = &self.index.db.cf_handle("outpoint_to_utxo_entry")
        .ok_or_else(|| anyhow!("Failed to open column family 'outpoint_to_utxo_entry'"))?;

      for (outpoint, utxo_entry) in utxo_cache {
        if Index::is_special_outpoint(outpoint) {
          // Don't store special outpoints
          continue;
        }

        /*if !utxo_entry.has_inscriptions() {
          // Don't store empty entries
          continue;
        }

        // if the outpoint is already in the database, do not overwrite it
        if outpoint_to_utxo_entry.get(&outpoint.store())?.is_some() {
          continue;
        }*/

        self.index.db.put_cf_opt(outpoint_to_utxo_entry, &outpoint.store(), utxo_entry.vec, &self.index.write_options)?;
      }
    }

    println!("Prepared db in {} ms", st_tm.elapsed().as_millis());
    let st_tm_2 = Instant::now();

    self.outputs_traversed = 0;
    self.sat_ranges_since_flush = 0;

    let mut flush_opts = rocksdb::FlushOptions::default();
    flush_opts.set_wait(true);

    let cfs = vec! [
      self.index.db.cf_handle("height_to_block_header")
        .ok_or_else(|| anyhow!("Failed to open column family 'height_to_block_header'"))?,
      self.index.db.cf_handle("height_to_last_sequence_number")
        .ok_or_else(|| anyhow!("Failed to open column family 'height_to_last_sequence_number'"))?,
      self.index.db.cf_handle("outpoint_to_utxo_entry")
        .ok_or_else(|| anyhow!("Failed to open column family 'outpoint_to_utxo_entry'"))?,
      self.index.db.cf_handle("inscription_id_to_sequence_number")
        .ok_or_else(|| anyhow!("Failed to open column family 'inscription_id_to_sequence_number'"))?,
      self.index.db.cf_handle("inscription_number_to_sequence_number")
        .ok_or_else(|| anyhow!("Failed to open column family 'inscription_number_to_sequence_number'"))?,
      self.index.db.cf_handle("inscription_id_to_txcnt")
        .ok_or_else(|| anyhow!("Failed to open column family 'inscription_id_to_txcnt'"))?,
      self.index.db.cf_handle("sequence_number_to_inscription_entry")
        .ok_or_else(|| anyhow!("Failed to open column family 'sequence_number_to_inscription_entry'"))?,
      self.index.db.cf_handle("statistic_to_count")
        .ok_or_else(|| anyhow!("Failed to open column family 'statistic_to_count'"))?,
      self.index.db.cf_handle("ord_transfers")
        .ok_or_else(|| anyhow!("Failed to open column family 'ord_transfers'"))?,
      self.index.db.cf_handle("ord_inscription_info")
        .ok_or_else(|| anyhow!("Failed to open column family 'ord_inscription_info'"))?,
      self.index.db.cf_handle("ord_index_stats")
        .ok_or_else(|| anyhow!("Failed to open column family 'ord_index_stats'"))?,
    ];

    //self.index.db.flush_opt(&flush_opts)?;
    self.index.db.flush_cfs_opt(&cfs, &flush_opts)?;

    println!("First commit done in {} ms", st_tm_2.elapsed().as_millis());
    let st_tm_3 = Instant::now();

    // Commit twice since due to a bug redb will only reuse pages freed in the
    // transaction before last.
    self.index.begin_write()?.commit()?;

    Reorg::update_savepoints(self.index, self.height)?;

    println!("Savepoints updated in {} ms", st_tm_3.elapsed().as_millis());

    Ok(())
  }
}
