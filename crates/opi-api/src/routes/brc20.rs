//! The brc20 module's routes (api/routes/brc20.js). It registers none of
//! its own, so paths under `/v1/brc20` answer finalhandler's 404.

use {crate::db::Db, axum::Router};

pub fn routes(router: Router, _db: Db) -> Router {
    router
}
