//! JavaScript number conversions: `Number#toString()`, `Number(string)`,
//! `parseInt(string)` and `parseFloat(string)`.

/// `String(n)`: the shortest digits that round-trip, laid out by the
/// ECMAScript Number::toString rules (exponent form below 1e-6 and from 1e21).
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n < 0.0 {
        return format!("-{}", number_to_string(-n));
    }
    // Rust's `{:e}` prints the shortest round-tripping digits.
    let scientific = format!("{n:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` always has an exponent");
    let exponent: i32 = exponent.parse().expect("`{:e}` exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // the value is 0.digits * 10^point
    let point = exponent + 1;
    if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        let (int, frac) = digits.split_at(point as usize);
        format!("{int}.{frac}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let e = point - 1;
        let sign = if e < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{first}e{sign}{}", e.abs())
        } else {
            format!("{first}.{rest}e{sign}{}", e.abs())
        }
    }
}

/// JavaScript's WhiteSpace and LineTerminator characters, which `trim` and
/// the string-to-number conversions skip.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

fn js_trim_start(s: &str) -> &str {
    s.trim_start_matches(is_js_whitespace)
}

/// The length of the longest prefix of `s` that is a StrDecimalLiteral
/// without `Infinity`: `[+-]? (digits [. digits?] | . digits) ([eE] [+-]? digits)?`.
fn decimal_prefix_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let digits_from = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_end = digits_from(i);
    let int_digits = int_end - i;
    i = int_end;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        let frac_end = digits_from(i + 1);
        frac_digits = frac_end - (i + 1);
        if int_digits > 0 || frac_digits > 0 {
            i = frac_end;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_end = digits_from(j);
        if exp_end > j {
            i = exp_end;
        }
    }
    Some(i)
}

/// The value of `digits` in `radix`, rounded once to the nearest double.
fn radix_value(digits: &str, radix: u32) -> f64 {
    let mut exact: u128 = 0;
    for (i, c) in digits.chars().enumerate() {
        let d = c.to_digit(radix).expect("caller checked the digits");
        match exact
            .checked_mul(radix.into())
            .and_then(|v| v.checked_add(d.into()))
        {
            Some(v) => exact = v,
            None => {
                // beyond 128 bits: continue in floating point
                let mut value = exact as f64;
                for c in digits.chars().skip(i) {
                    value = value * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0));
                }
                return value;
            }
        }
    }
    exact as f64
}

/// `Number(string)` (StringToNumber).
pub fn string_to_number(s: &str) -> f64 {
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let b = t.as_bytes();
    if b.len() >= 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => Some(16),
            b'o' | b'O' => Some(8),
            b'b' | b'B' => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            let digits = &t[2..];
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return radix_value(digits, radix);
        }
    }
    match decimal_prefix_len(t) {
        Some(len) if len == t.len() => t.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// `parseInt(string)` with no radix: decimal, or hexadecimal after `0x`.
pub fn parse_int_str(s: &str) -> f64 {
    parse_int(s, false)
}

/// `parseInt(string, 10)`.
pub fn parse_int_base10(s: &str) -> f64 {
    parse_int(s, true)
}

fn parse_int(s: &str, base10: bool) -> f64 {
    let t = js_trim_start(s);
    let (negative, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let (radix, rest) = if !base10 && (rest.starts_with("0x") || rest.starts_with("0X")) {
        (16, &rest[2..])
    } else {
        (10, rest)
    };
    let len = rest
        .char_indices()
        .find(|(_, c)| !c.is_digit(radix))
        .map_or(rest.len(), |(i, _)| i);
    if len == 0 {
        return f64::NAN;
    }
    let digits = &rest[..len];
    let value = if radix == 10 {
        digits.parse().unwrap_or(f64::NAN)
    } else {
        radix_value(digits, radix)
    };
    if negative { -value } else { value }
}

/// `parseFloat(string)`: the longest decimal prefix after leading whitespace.
pub fn parse_float_str(s: &str) -> f64 {
    let t = js_trim_start(s);
    let (sign, unsigned) = match t.as_bytes().first() {
        Some(b'-') => (-1.0, &t[1..]),
        Some(b'+') => (1.0, &t[1..]),
        _ => (1.0, t),
    };
    if unsigned.starts_with("Infinity") {
        return sign * f64::INFINITY;
    }
    match decimal_prefix_len(t) {
        Some(len) => t[..len].parse().unwrap_or(f64::NAN),
        None => f64::NAN,
    }
}
