use {super::*, updater::BlockData};

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
  Unrecoverable,
}

impl Display for Error {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    match self {
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
        Err(anyhow!(reorg::Error::Unrecoverable))
      }
      _ => Ok(()),
    }
  }

  pub(crate) fn is_savepoint_required(_index: &Index, _height: u32) -> Result<bool> {
    Ok(false)
  }

  pub(crate) fn update_savepoints(_index: &Index, _height: u32) -> Result {
    Ok(())
  }
}
