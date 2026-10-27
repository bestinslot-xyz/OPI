/// `decodeURIComponent(s)`; `None` where it throws URIError: a `%` without
/// two hex digits, or escapes that are not well-formed UTF-8 (overlong
/// forms, surrogates and truncated sequences included).
pub fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let escaped = |i: usize| -> Option<u8> {
        let hex = s.get(i + 1..i + 3)?;
        if bytes[i] != b'%' || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u8::from_str_radix(hex, 16).ok()
    };
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let c = s[i..].chars().next().expect("i is on a char boundary");
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let lead = escaped(i)?;
        i += 3;
        let len = match lead {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return None,
        };
        let mut sequence = vec![lead];
        for _ in 1..len {
            if i >= bytes.len() {
                return None;
            }
            sequence.push(escaped(i)?);
            i += 3;
        }
        out.push_str(std::str::from_utf8(&sequence).ok()?);
    }
    Some(out)
}
