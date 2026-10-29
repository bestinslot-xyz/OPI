//! [`ModuleRoutes`], the only way a module registers its routes.

use {
    crate::db::Db,
    utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter},
};

/// A module's routes, each with its OpenAPI operation. It takes routes only
/// through [`ModuleRoutes::routes`], which takes what `routes!(handler)`
/// builds from the handler's `#[utoipa::path]`, so a module route without
/// an operation does not compile. Its field is private to this module, so
/// a route module cannot reach the router inside.
pub struct ModuleRoutes<S = Db>(OpenApiRouter<S>);

impl<S: Clone + Send + Sync + 'static> ModuleRoutes<S> {
    pub fn new() -> Self {
        ModuleRoutes(OpenApiRouter::default())
    }

    /// Adds the route and the operation of `routes!(handler)`.
    pub fn routes(self, routes: UtoipaMethodRouter<S>) -> Self {
        ModuleRoutes(self.0.routes(routes))
    }

    /// The routes, for the app to merge.
    pub fn into_inner(self) -> OpenApiRouter<S> {
        self.0
    }
}

impl<S: Clone + Send + Sync + 'static> Default for ModuleRoutes<S> {
    fn default() -> Self {
        ModuleRoutes::new()
    }
}
