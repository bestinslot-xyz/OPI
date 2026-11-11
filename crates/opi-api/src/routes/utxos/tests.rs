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
