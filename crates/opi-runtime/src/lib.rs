//! Drives every OPI module over the journal: one supervised task per module,
//! each following the journal at its own pace and recovering on its own.

mod scheduler;
mod supervisor;

pub use {
    opi_core::MAX_ROLLBACK_DEPTH,
    supervisor::{ModuleFactory, Runtime, RuntimeOptions},
};
