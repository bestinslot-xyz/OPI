use super::*;
use crate::index::reorg::Error as ReorgError;
use crate::index::reorg::Reorg;

pub(crate) fn run(settings: Settings) -> SubcommandResult {
  loop {
    let index = Index::open(&settings)?;

    let res = index.update();

    match res {
      Err(err) => {
        match err.downcast_ref() {
          Some(&ReorgError::Recoverable { height, depth }) => {
            let index_path = index.path.clone();
            drop(index); // Ensure the index and dbs are closed before handling the reorg
            Reorg::handle_reorg(&index_path, height, depth)?;
          }
          _ => {
            break;
          }
        }
      }
      _ => {
        break;
      }
    }
  }

  Ok(None)
}
