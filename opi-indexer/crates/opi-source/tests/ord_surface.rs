mod support;

use {
    opi_source::ord_settings,
    ord::{Chain, Index, ParsedEnvelope, index::event::Event},
    support::Harness,
};

#[test]
fn ord_index_emits_creation_and_exposes_entry() {
    let harness = Harness::new();
    let tmp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);

    let settings = ord_settings(Chain::Regtest, tmp.path(), &harness.core.url()).unwrap();
    let index = Index::open_with_event_sender(&settings, Some(tx)).unwrap();

    let id = harness.inscribe(b"{\"p\":\"brc-20\"}", "text/plain");
    harness.mine(1);
    index.update().unwrap();

    let mut created = None;
    while let Ok(event) = rx.try_recv() {
        if let Event::InscriptionCreated {
            inscription_id,
            location,
            ..
        } = event
        {
            created = Some((inscription_id, location));
        }
    }
    let (event_id, location) = created.expect("InscriptionCreated event");
    assert_eq!(event_id, id);
    assert!(location.is_some());

    assert_eq!(
        index
            .get_inscription_entry(id)
            .unwrap()
            .unwrap()
            .inscription_number,
        0
    );

    let height = u32::try_from(harness.core.height()).unwrap();
    assert!(index.block_hash(Some(height)).unwrap().is_some());

    let reveal_tx = harness.core.tx_by_id(id.txid);
    assert_eq!(
        ParsedEnvelope::from_transaction(&reveal_tx)[0]
            .payload
            .body
            .as_deref(),
        Some(b"{\"p\":\"brc-20\"}".as_slice())
    );
}
