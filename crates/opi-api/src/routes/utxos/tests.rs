use super::*;

fn query(pairs: &[(&str, &str)]) -> JsObject {
    let mut query = JsObject::new();
    for (key, value) in pairs {
        query.set(*key, *value);
    }
    query
}

#[test]
fn type_defaults_to_all_and_refuses_others() {
    assert_eq!(Kind::parse(&query(&[])).unwrap(), Kind::All);
    assert_eq!(Kind::parse(&query(&[("type", "")])).unwrap(), Kind::All);
    for kind in [Kind::All, Kind::Cardinal, Kind::Inscribed, Kind::Runic] {
        assert_eq!(Kind::parse(&query(&[("type", kind.name())])).unwrap(), kind);
    }
    let err = Kind::parse(&query(&[("type", "ordinal")])).unwrap_err();
    assert_eq!(err.0, "type must be all, cardinal, inscribed or runic");
}

#[test]
fn cardinal_keeps_plain_outputs_and_used_transfers_only() {
    let plain = Carries::default();
    let used_transfer = Carries {
        inscriptions: 1,
        live_inscriptions: 0,
        runes: 0,
    };
    let live = Carries {
        inscriptions: 2,
        live_inscriptions: 1,
        runes: 0,
    };
    let runic = Carries { runes: 1, ..plain };
    let at = Some(100);
    assert!(keeps(Kind::Cardinal, plain, 100, at));
    assert!(keeps(Kind::Cardinal, used_transfer, 50, at));
    assert!(!keeps(Kind::Cardinal, live, 50, at));
    assert!(!keeps(Kind::Cardinal, runic, 50, at));
    // Above the lowest module height a module may not have seen the block.
    assert!(!keeps(Kind::Cardinal, plain, 101, at));
    assert!(!keeps(Kind::Cardinal, plain, 1, None));
}

#[test]
fn the_other_types_filter_by_what_the_output_carries() {
    let plain = Carries::default();
    let inscribed = Carries {
        inscriptions: 1,
        live_inscriptions: 1,
        runes: 0,
    };
    let runic = Carries { runes: 2, ..plain };
    for carries in [plain, inscribed, runic] {
        assert!(keeps(Kind::All, carries, 500, Some(1)));
    }
    assert!(keeps(Kind::Inscribed, inscribed, 500, None));
    assert!(!keeps(Kind::Inscribed, runic, 1, Some(1)));
    assert!(keeps(Kind::Runic, runic, 500, None));
    assert!(!keeps(Kind::Runic, inscribed, 1, Some(1)));
}

#[test]
fn mempool_is_a_boolean_defaulting_to_false() {
    assert!(!parse_mempool(&query(&[])).unwrap());
    assert!(!parse_mempool(&query(&[("mempool", "false")])).unwrap());
    assert!(parse_mempool(&query(&[("mempool", "true")])).unwrap());
    assert_eq!(
        parse_mempool(&query(&[("mempool", "1")])).unwrap_err().0,
        "mempool must be true or false"
    );
}

#[test]
fn a_mempool_output_is_cardinal_only_when_not_pending() {
    assert!(keeps_mempool(Kind::All, true));
    assert!(keeps_mempool(Kind::Cardinal, false));
    assert!(!keeps_mempool(Kind::Cardinal, true));
    for kind in [Kind::Inscribed, Kind::Runic] {
        assert!(!keeps_mempool(kind, false));
    }
}

#[test]
fn a_mempool_output_is_pending_unless_its_lineage_is_known_clean() {
    use bitcoin::{OutPoint, Txid, hashes::Hash};
    let input = OutPoint {
        txid: Txid::from_byte_array([1; 32]),
        vout: 0,
    };
    let lineage = Lineage {
        marked: false,
        confirmed_inputs: vec![input],
    };
    let clean = InputFacts {
        height: Some(90),
        carries_inscriptions: false,
        carries_runes: false,
    };
    let at = Some(100);
    assert!(!is_pending(&lineage, |_| clean, at));
    assert!(is_pending(
        &Lineage {
            marked: true,
            ..lineage.clone()
        },
        |_| clean,
        at
    ));
    for facts in [
        InputFacts {
            carries_inscriptions: true,
            ..clean
        },
        InputFacts {
            carries_runes: true,
            ..clean
        },
        // the utxos module does not hold the input
        InputFacts {
            height: None,
            ..clean
        },
        // created above a module's height: its contents may be unknown
        InputFacts {
            height: Some(101),
            ..clean
        },
    ] {
        assert!(is_pending(&lineage, |_| facts, at), "{facts:?}");
    }
    assert!(is_pending(&lineage, |_| clean, None));
    // a lineage that spends nothing confirmed and opens nothing is clean
    assert!(!is_pending(&Lineage::default(), |_| clean, at));
}
