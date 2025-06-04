use tokio::runtime::Runtime;

use super::*;

pub(crate) fn run(settings: Settings) -> SubcommandResult {
  let index = Index::open(&settings)?;

  /* let index_path = index.path.clone();
  let rt = Runtime::new().unwrap();
  // run rpc_server::start_rpc_server(index_path).await in a thread
  rt.block_on(async move {
    if let Err(e) = rpc_server::start_rpc_server(index_path).await {
      eprintln!("Failed to start RPC server: {}", e);
    }
  }); */
  
  index.update()?;

  Ok(None)
}
