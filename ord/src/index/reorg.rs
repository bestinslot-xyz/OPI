use {super::*, updater::BlockData};

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
  Recoverable { height: u32, depth: u32 },
  Unrecoverable,
}

impl Display for Error {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    match self {
      Self::Recoverable { height, depth } => {
        write!(f, "{depth} block deep reorg detected at height {height}")
      }
      Self::Unrecoverable => write!(f, "unrecoverable reorg detected"),
    }
  }
}

impl std::error::Error for Error {}

pub(crate) struct Reorg {}

impl Reorg {
  pub(crate) fn detect_reorg(block: &BlockData, height: u32, index: &Index) -> Result {
    let bitcoind_prev_blockhash = block.header.prev_blockhash;

    match index.block_hash(height.checked_sub(1))? {
      Some(index_prev_blockhash) if index_prev_blockhash == bitcoind_prev_blockhash => Ok(()),
      Some(index_prev_blockhash) if index_prev_blockhash != bitcoind_prev_blockhash => {
        let savepoint_interval = u32::try_from(index.settings.savepoint_interval()).unwrap();
        let max_savepoints = u32::try_from(index.settings.max_savepoints()).unwrap();
        let max_recoverable_reorg_depth =
          (max_savepoints - 1) * savepoint_interval + height % savepoint_interval;

        for depth in 1..max_recoverable_reorg_depth {
          let index_block_hash = index.block_hash(height.checked_sub(depth))?;
          let bitcoind_block_hash = index
            .client
            .get_block_hash(u64::from(height.saturating_sub(depth)))
            .into_option()?;

          if index_block_hash == bitcoind_block_hash {
            return Err(anyhow!(reorg::Error::Recoverable { height, depth }));
          }
        }

        Err(anyhow!(reorg::Error::Unrecoverable))
      }
      _ => Ok(()),
    }
  }

  pub(crate) fn is_savepoint_required(index: &Index, height: u32) -> Result<bool> {
    let height = u64::from(height);

    let statistic_to_count = index.db.cf_handle("statistic_to_count")
      .ok_or_else(|| anyhow!("Failed to open column family 'statistic_to_count'"))?;

    let last_savepoint_height = index.db
      .get_cf(statistic_to_count, &Statistic::LastSavepointHeight.key().to_be_bytes())?
      .map(|last_savepoint_height| u64::from(u32::from_be_bytes(last_savepoint_height.try_into().unwrap())))
      .unwrap_or(0);

    let blocks = index.client.get_blockchain_info()?.headers;

    let savepoint_interval = u64::try_from(index.settings.savepoint_interval()).unwrap();
    let max_savepoints = u64::try_from(index.settings.max_savepoints()).unwrap();

    let result = (height < savepoint_interval
      || height.saturating_sub(last_savepoint_height) >= savepoint_interval)
      && blocks.saturating_sub(height) <= savepoint_interval * max_savepoints + 1;

    log::trace!(
      "is_savepoint_required={}: height={}, last_savepoint_height={}, blocks={}",
      result,
      height,
      last_savepoint_height,
      blocks
    );

    Ok(result)
  }

  pub(crate) fn handle_reorg(index: &Index, height: u32, depth: u32) -> Result {
    println!("rolling back database after reorg of depth {depth} at height {height}");

    let backup_opts = BackupEngineOptions::new(&index.path.join("backup"))?;
    let mut backup_engine = BackupEngine::open(&backup_opts, &rocksdb::Env::new()?)?;

    let backups = backup_engine.get_backup_info();
    // get the oldest backup
    let oldest_backup = backups
      .iter()
      .min_by_key(|backup| backup.backup_id)
      .ok_or_else(|| anyhow!("No backups found"))?;
    let backup_id = oldest_backup.backup_id;

    println!("restoring backup with id {}", backup_id);
    let db_dir = index.path.join("index.db");
    let wal_dir = index.path.join("index.db");
    let opts = rocksdb::backup::RestoreOptions::default();

    backup_engine.restore_from_backup(db_dir, wal_dir, &opts, backup_id)?;

    println!(
      "successfully rolled back database to height {}",
      index.block_count()?
    );

    Ok(())
  }

  pub(crate) fn update_savepoints(index: &Index, height: u32) -> Result {
    if Self::is_savepoint_required(index, height)? {
      let backup_opts = BackupEngineOptions::new(&index.path.join("backup"))?;
      let mut backup_engine = BackupEngine::open(&backup_opts, &rocksdb::Env::new()?)?;

      backup_engine.purge_old_backups(index.settings.max_savepoints() - 1)?;

      println!("Creating savepoint at height {}", height);

      backup_engine.create_new_backup(&index.db)?;

      println!("Savepoint created successfully");

      let statistic_to_count = index.db.cf_handle("statistic_to_count")
        .ok_or_else(|| anyhow!("Failed to open column family 'statistic_to_count'"))?;

      index.db
        .put_cf_opt(statistic_to_count, &Statistic::LastSavepointHeight.key().to_be_bytes(), height.to_be_bytes(), &index.write_options)?;

      let mut flush_opts = rocksdb::FlushOptions::default();
      flush_opts.set_wait(true);

      index.db.flush_cf_opt(statistic_to_count, &flush_opts)?;
    }

    Ok(())
  }
}
