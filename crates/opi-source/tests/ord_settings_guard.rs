//! Sets `ORD_INTEGRATION_TEST` for the whole process, so it is the only test
//! in this binary.

mod support;

use {
    opi_source::driver::spawn_source,
    support::{Harness, source_config},
};

#[test]
fn startup_refuses_ord_integration_test() {
    // SAFETY: this binary runs one test, and no other thread reads the
    // environment yet.
    unsafe { std::env::set_var("ORD_INTEGRATION_TEST", "1") };

    let harness = Harness::new();
    let tmp = tempfile::tempdir().unwrap();
    let config = source_config(tmp.path(), harness.core.url(), 2);
    let journal_path = config.journal_dir.join(opi_source::journal::META_FILE);

    let err = spawn_source(config).err().expect("startup refuses");
    assert!(format!("{err:#}").contains("integration_test"), "{err:#}");
    assert!(!journal_path.exists(), "a refused start leaves no journal");
}
