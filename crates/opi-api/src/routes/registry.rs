//! [`ModuleRoutes`], the only way a module registers its routes.

use {
    crate::db::Db,
    utoipa::openapi::{OpenApi, RefOr, schema::Schema},
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
        check_components(self.0.get_openapi(), &routes.0);
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

/// Panics when a schema of `added` takes the name of a different schema of
/// `existing`: the document keeps one schema per name, so the second would
/// replace the first.
pub fn check_components(existing: &OpenApi, added: &[(String, RefOr<Schema>)]) {
    let Some(components) = &existing.components else {
        return;
    };
    for (name, schema) in added {
        if let Some(known) = components.schemas.get(name) {
            assert!(
                known == schema,
                "two schemas are named {name}; give one another name"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        utoipa::{PartialSchema, openapi::ComponentsBuilder},
    };

    #[test]
    #[should_panic(expected = "two schemas are named Thing")]
    fn a_schema_name_names_one_schema() {
        let mut existing = OpenApi::default();
        existing.components = Some(
            ComponentsBuilder::new()
                .schema("Thing", String::schema())
                .build(),
        );
        check_components(&existing, &[("Thing".into(), i32::schema())]);
    }

    #[test]
    fn the_same_schema_may_be_added_again() {
        let mut existing = OpenApi::default();
        existing.components = Some(
            ComponentsBuilder::new()
                .schema("Thing", String::schema())
                .build(),
        );
        check_components(&existing, &[("Thing".into(), String::schema())]);
    }
}
