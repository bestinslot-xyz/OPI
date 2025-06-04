use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;

use hyper::Method;
use jsonrpsee::core::middleware::RpcServiceBuilder;
use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::server::{Server, ServerHandle};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned};
use rocksdb::{Options, DB};
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};

struct RpcServer {
  ord_transfers: DB,
  ord_inscription_info: DB,
  ord_index_stats: DB,
}

#[rpc(server, client)]
pub trait Brc20Api {
    /// Get BRC-20 transactions in a block.
    #[method(name = "getBlockIndexTimes")]
    async fn get_block_index_times(&self, block_height: u32) -> RpcResult<Option<(u128, u128, u128)>>;
}

pub fn wrap_rpc_error(error: Box<dyn Error>) -> ErrorObject<'static> {
    ErrorObjectOwned::owned(400, error.to_string(), None::<String>)
}

fn get_times_from_stat(
    stat: Option<Vec<u8>>,
) -> Option<(u128, u128, u128)> {
    if stat.is_none() {
        return None;
    }
    let stat = stat?;
    if stat.len() < 48 {
        return None;
    }
    let mut iter = stat.chunks(16);
    let fetch_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
    let index_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
    let commit_tm = u128::from_be_bytes(iter.next()?.try_into().ok()?);
    Some((fetch_tm, index_tm, commit_tm))
}

#[async_trait]
impl Brc20ApiServer for RpcServer {
    async fn get_block_index_times(
        &self,
        block_height: u32,
    ) -> RpcResult<Option<(u128, u128, u128)>> {
        Ok(self.ord_index_stats.get(&block_height.to_be_bytes())
            .map(|time| get_times_from_stat(time))
            .unwrap())
    }
}

pub async fn start_rpc_server(
    index_path: PathBuf,
) -> Result<ServerHandle, Box<dyn Error>> {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_max_open_files(256);
    
    let ord_transfers_path = index_path.join("ord_transfers.db");
    let ord_transfers = DB::open_for_read_only(&opts, &ord_transfers_path, true)?;

    let ord_inscription_info_path = index_path.join("ord_inscription_info.db");
    let ord_inscription_info = DB::open_for_read_only(&opts, &ord_inscription_info_path, true)?;

    let ord_index_stats_path = index_path.join("ord_index_stats.db");
    let ord_index_stats = DB::open_for_read_only(&opts, &ord_index_stats_path, true)?;

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
        ord_transfers,
        ord_inscription_info,
        ord_index_stats,
    }.into_rpc();

    let handle = Server::builder()
        .set_http_middleware(http_middleware)
        .set_rpc_middleware(rpc_middleware)
        .build("127.0.0.1:3030".parse::<SocketAddr>()?)
        .await?
        .start(module);

    println!("RPC server started at http://127.0.0.1:3030");

    Ok(handle)
}