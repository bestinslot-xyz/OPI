//! `JSON.stringify` and `JSON.parse` over [`JsValue`].

use {
    super::{JsError, JsObject, JsValue, number::number_to_string},
    std::fmt::Write,
};

/// `JSON.stringify(value)`; `None` where it returns `undefined`.
pub fn json_stringify(value: &JsValue) -> Option<String> {
    if value.is_undefined() {
        return None;
    }
    let mut out = String::new();
    write_value(&mut out, value);
    Some(out)
}

fn write_value(out: &mut String, value: &JsValue) {
    match value {
        JsValue::Undefined | JsValue::Null => out.push_str("null"),
        JsValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsValue::Number(n) if n.is_finite() => out.push_str(&number_to_string(*n)),
        JsValue::Number(_) => out.push_str("null"),
        JsValue::String(s) => write_string(out, s),
        JsValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        JsValue::Object(obj) => {
            out.push('{');
            let mut first = true;
            for (key, item) in obj.iter().filter(|(_, v)| !v.is_undefined()) {
                if !first {
                    out.push(',');
                }
                first = false;
                write_string(out, key);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

/// QuoteJSONString: escapes `"`, `\` and the control characters below
/// U+0020; everything else is written as it is.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `JSON.parse(text)`. Numbers become doubles; an object keeps the last
/// value of a repeated key at the key's first position. A lone surrogate
/// escape, which a Rust string cannot hold, is a SyntaxError.
pub fn json_parse(text: &str) -> Result<JsValue, JsError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("unexpected non-whitespace character after JSON"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> JsError {
        JsError::syntax_error(format!("{what} at position {}", self.pos))
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.text[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<JsValue, JsError> {
        match self.bytes.get(self.pos) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(JsValue::String(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ if self.eat("true") => Ok(JsValue::Bool(true)),
            _ if self.eat("false") => Ok(JsValue::Bool(false)),
            _ if self.eat("null") => Ok(JsValue::Null),
            _ => Err(self.error("unexpected token")),
        }
    }

    fn object(&mut self) -> Result<JsValue, JsError> {
        self.pos += 1;
        let mut obj = JsObject::new();
        self.skip_ws();
        if self.eat("}") {
            return Ok(JsValue::Object(obj));
        }
        loop {
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.error("expected a property name"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(":") {
                return Err(self.error("expected ':'"));
            }
            self.skip_ws();
            let value = self.value()?;
            obj.set(key, value);
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("}") {
                return Ok(JsValue::Object(obj));
            }
            return Err(self.error("expected ',' or '}'"));
        }
    }

    fn array(&mut self) -> Result<JsValue, JsError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat("]") {
            return Ok(JsValue::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("]") {
                return Ok(JsValue::Array(items));
            }
            return Err(self.error("expected ',' or ']'"));
        }
    }

    fn number(&mut self) -> Result<JsValue, JsError> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while p.bytes.get(p.pos).is_some_and(u8::is_ascii_digit) {
                p.pos += 1;
            }
            p.pos - from
        };
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("no number after minus sign")),
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if digits(self) == 0 {
                return Err(self.error("unterminated fractional number"));
            }
        }
        if let Some(b'e' | b'E') = self.bytes.get(self.pos) {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.bytes.get(self.pos) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                return Err(self.error("exponent part is missing a number"));
            }
        }
        let literal = &self.text[start..self.pos];
        Ok(JsValue::Number(
            literal.parse().expect("a JSON number literal parses"),
        ))
    }

    fn hex4(&mut self) -> Result<u32, JsError> {
        let hex = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("bad Unicode escape"))?;
        self.pos += 4;
        Ok(u32::from_str_radix(hex, 16).expect("checked hex digits"))
    }

    fn string(&mut self) -> Result<String, JsError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = &self.text[self.pos..];
            let Some(c) = rest.chars().next() else {
                return Err(self.error("unterminated string"));
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let escape = self.bytes.get(self.pos).copied();
                    self.pos += 1;
                    match escape {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            let unit = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&unit) && self.eat("\\u") {
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(self.error("lone surrogate in string"));
                                }
                                0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                unit
                            };
                            out.push(
                                char::from_u32(code)
                                    .ok_or_else(|| self.error("lone surrogate in string"))?,
                            );
                        }
                        _ => return Err(self.error("bad escaped character")),
                    }
                }
                c if c < '\u{20}' => return Err(self.error("bad control character in string")),
                c => out.push(c),
            }
        }
    }
}
