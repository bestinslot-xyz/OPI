use {
    super::Db,
    crate::{config::Module, js::JsValue, params::IntRules},
};

#[tokio::test]
async fn a_fake_answers_heights_and_runs_no_queries() {
    let db = Db::fake(|module| match module {
        Module::Sns => anyhow::bail!("down"),
        _ => Ok(JsValue::from(7)),
    });
    assert_eq!(
        db.latest_block_height(Module::Brc20).await.unwrap(),
        JsValue::from(7)
    );
    assert!(db.latest_block_height(Module::Sns).await.is_err());
    assert!(db.pool().is_err());
    assert_eq!(db.int_rules().await.unwrap(), IntRules::Pg14);
}

#[tokio::test]
async fn int_rules_fail_while_the_server_is_unreachable() {
    let options = "postgres://u@127.0.0.1:1/db".parse().unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy_with(options);
    let db = Db::from_pool(pool);
    // each call tries again; none caches a guess
    assert!(db.int_rules().await.is_err());
    assert!(db.int_rules().await.is_err());
}
