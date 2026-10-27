use {
    super::Db,
    crate::{config::Module, js::JsValue},
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
}
