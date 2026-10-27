//! The runes module's routes (api/routes/runes.js). It registers none of
//! its own, so paths under `/v1/runes` answer finalhandler's 404.

use {crate::db::Db, axum::Router};

pub fn routes(router: Router, _db: Db) -> Router {
    router
}
