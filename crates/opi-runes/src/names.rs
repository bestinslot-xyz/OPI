//! Rune names and ids as the API reads them, through ord's own parsers
//! (`ordinals`): `runes_id_to_entry.rune_name` holds a name's letters
//! without spacers and `spacers` the bitfield ord keeps, where bit `i` puts a
//! spacer after letter `i`.

use std::str::FromStr;

use ordinals::{Rune, RuneId, SpacedRune};

/// A rune as a request names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuneRef {
    /// A rune id, `block:tx` in ord's form.
    Id(String),
    /// A rune name's letters. A rune's identity is its unspaced name, so
    /// the spacers a request writes do not take part.
    Name(String),
}

/// The rune `text` names: a rune id (`block:tx`) or a name, spaced (`•` or
/// `.`) with any spacing or not, in any ASCII case. `None` for text that is
/// neither.
pub fn parse_rune(text: &str) -> Option<RuneRef> {
    if text.contains(':') {
        return RuneId::from_str(text)
            .ok()
            .map(|id| RuneRef::Id(id.to_string()));
    }
    let spaced = SpacedRune::from_str(&text.to_ascii_uppercase()).ok()?;
    Some(RuneRef::Name(spaced.rune.to_string()))
}

/// The spaced form of a stored name and its spacers, as ord displays it
/// (`UNCOMMON•GOODS`); the stored name itself when it is not a rune name.
pub fn spaced_name(name: &str, spacers: i64) -> String {
    match (Rune::from_str(name), u32::try_from(spacers)) {
        (Ok(rune), Ok(spacers)) => SpacedRune::new(rune, spacers).to_string(),
        _ => name.to_string(),
    }
}

/// A name prefix: its letters, and the spacer bits a matching name has
/// within `mask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePrefix {
    pub letters: String,
    /// The spacer positions the prefix fixes: none for a prefix written
    /// without spacers, otherwise every gap it covers.
    pub mask: i64,
    pub spacers: i64,
}

/// The prefix `text` writes, in any ASCII case: letters, with spacers (`•`
/// or `.`) between them and optionally one at the end. A prefix with a
/// spacer matches the spaced form (`UNCOMMON•G` finds `UNCOMMON•GOODS`, not
/// `UNCOMMONG•OODS`), one without matches the letters. `None` for any other
/// text, which no name starts with.
pub fn name_prefix(text: &str) -> Option<NamePrefix> {
    let mut letters = String::new();
    let mut spacers: u64 = 0;
    let mut trailing = false;
    for c in text.chars() {
        match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => {
                letters.push(c);
                trailing = false;
            }
            '.' | '•' => {
                if letters.is_empty() || trailing {
                    return None;
                }
                let bit = 1u64.checked_shl(letters.len() as u32 - 1)?;
                spacers |= bit;
                trailing = true;
            }
            _ => return None,
        }
    }
    if letters.is_empty() {
        return None;
    }
    // the gaps after letters 0..n-1 and, with a trailing spacer, after the
    // last letter too; a gap past bit 62 lies beyond any rune name
    let gaps = letters.len() - usize::from(!trailing);
    let mask = if spacers == 0 {
        0
    } else {
        1u64.checked_shl(gaps as u32)
            .map_or(u64::MAX, |bit| bit - 1)
    };
    Some(NamePrefix {
        letters,
        mask: (mask & i64::MAX as u64) as i64,
        spacers: (spacers & i64::MAX as u64) as i64,
    })
}

/// The symbol as `runes_id_to_entry.symbol` holds it: the hex of its UTF-8
/// bytes zero-padded to 4 bytes ([`crate::symbol_hex`]), or the character
/// itself (mainnet's seeded `1:0`).
pub fn symbol_char(stored: &str) -> Option<char> {
    let mut chars = stored.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c);
    }
    let bytes = hex::decode(stored).ok()?;
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runes_parse_by_id_or_name() {
        assert_eq!(parse_rune("840000:3"), Some(RuneRef::Id("840000:3".into())));
        assert_eq!(parse_rune("01:002"), Some(RuneRef::Id("1:2".into())));
        assert_eq!(parse_rune("1:"), None);
        assert_eq!(parse_rune("a:1"), None);
        // any spacing of the letters names the same rune
        for spelling in [
            "uncommongoods",
            "Uncommon•Goods",
            "UNCOMMON.GOODS",
            "U.N.C.OMMONGOODS",
        ] {
            assert_eq!(
                parse_rune(spelling),
                Some(RuneRef::Name("UNCOMMONGOODS".into())),
                "{spelling}"
            );
        }
        for bad in [
            "",
            "A..B",
            ".A",
            "A.",
            "AB1",
            "ß",
            "BCGDENLQRQWDSLRUGSNLBTMFIJAW",
        ] {
            assert_eq!(parse_rune(bad), None, "{bad}");
        }
    }

    #[test]
    fn spaced_names_show_their_spacers() {
        assert_eq!(spaced_name("UNCOMMONGOODS", 128), "UNCOMMON•GOODS");
        assert_eq!(spaced_name("ABC", 0b11), "A•B•C");
        assert_eq!(spaced_name("ABC", 0), "ABC");
        assert_eq!(spaced_name("not a rune", 1), "not a rune");
    }

    #[test]
    fn prefixes_fix_the_spacers_they_cover() {
        let prefix = |letters: &str, mask: i64, spacers: i64| {
            Some(NamePrefix {
                letters: letters.into(),
                mask,
                spacers,
            })
        };
        assert_eq!(name_prefix("uncommong"), prefix("UNCOMMONG", 0, 0));
        assert_eq!(name_prefix("UNCOMMON•G"), prefix("UNCOMMONG", 0xff, 0x80));
        assert_eq!(name_prefix("UNCOMMON•"), prefix("UNCOMMON", 0xff, 0x80));
        assert_eq!(name_prefix("A.B.C"), prefix("ABC", 0b11, 0b11));
        for bad in ["", "•", ".A", "A..", "A•.", "A-B", "1"] {
            assert_eq!(name_prefix(bad), None, "{bad}");
        }
        assert!(name_prefix(&"A".repeat(100)).is_some());
        assert!(name_prefix(&"A.".repeat(70)).is_none());
    }

    #[test]
    fn symbols_decode_from_padded_hex() {
        assert_eq!(symbol_char("24000000"), Some('$'));
        assert_eq!(symbol_char("f09f8d86"), Some('🍆'));
        assert_eq!(symbol_char("e19ab900"), Some('ᚹ'));
        assert_eq!(symbol_char("⧉"), Some('⧉'));
        assert_eq!(symbol_char("00000000"), None);
        assert_eq!(symbol_char("zz"), None);
        assert_eq!(symbol_char(""), None);
    }
}
