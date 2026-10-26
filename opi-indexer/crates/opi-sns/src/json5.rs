//! JSON5 as SNS reads it: the JSON5 1.0 grammar as Python's json5 0.12.0
//! implements it (`json5/parser.py` and the value conversion in
//! `json5/lib.py`), with nesting capped at [`NESTING_LIMIT`].
//!
//! The grammar details that decide whether a document parses:
//! - whitespace is space, tab, U+000B, U+000C, U+00A0, U+FEFF, any Zs code
//!   point, line terminators (CR LF, CR, LF, U+2028, U+2029) and `//` / `/* */`
//!   comments;
//! - strings take single or double quotes and any character but the quote, a
//!   backslash or a line terminator; escapes are `\b \f \n \r \t \v \' \" \\`,
//!   `\0` not followed by a digit, `\xHH`, `\uHHHH` (each escape one code
//!   point, so surrogate pairs stay two lone surrogates), a backslash before a
//!   line terminator (removed), and a backslash before any other character
//!   except `x`, `u` and digits (that character);
//! - object keys are strings or identifiers: an ASCII letter, `$`, `_`, a
//!   letter (Ll, Lm, Lo, Lt, Lu, Nl) or `\uHHHH`, then those, digits, Mn, Mc,
//!   Nd, Pc, ZWNJ and ZWJ;
//! - numbers are an optional sign then a decimal literal not followed by an
//!   identifier character, `0x`/`0X` hex digits, `Infinity` or `NaN`. A
//!   literal fails the document when it does not convert: a sign before hex,
//!   a fraction or exponent without digits, or an integer of more than 4300
//!   digits;
//! - trailing commas are allowed in objects and arrays;
//! - a repeated key keeps its first position and takes its last value.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::unicode::in_ranges;
use crate::unicode_tables::{LL, LM, LO, LT, LU, MC, MN, ND, NL, PC, ZS};

/// The deepest nesting of arrays and objects that parses; deeper documents
/// fail.
pub const NESTING_LIMIT: usize = 128;

/// Digits beyond which a decimal integer fails to convert.
const MAX_INT_DIGITS: usize = 4300;

/// A parsed JSON5 value. Strings are code points and may hold lone
/// surrogates.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// A number; the SNS rules never read its value.
    Number,
    Str(Vec<u32>),
    List(Vec<Value>),
    /// Members in first-occurrence order.
    Object(Vec<(Vec<u32>, Value)>),
}

impl Value {
    /// The value under `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| cps_eq(k, key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Whether this is the string `s`.
    pub fn is_str(&self, s: &str) -> bool {
        matches!(self, Value::Str(cps) if cps_eq(cps, s))
    }
}

fn cps_eq(cps: &[u32], s: &str) -> bool {
    cps.iter().copied().eq(s.chars().map(u32::from))
}

/// The value of the JSON5 document `text`, or `None` when it does not parse.
pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser {
        s: text.chars().map(u32::from).collect(),
        pos: 0,
        depth: 0,
    };
    parser.sp();
    let value = parser.value().ok()?;
    parser.sp();
    (parser.pos == parser.s.len()).then_some(value)
}

struct Fail;

type Parsed<T> = Result<T, Fail>;

struct Parser {
    s: Vec<u32>,
    pos: usize,
    depth: usize,
}

const CR: u32 = '\r' as u32;
const LF: u32 = '\n' as u32;

fn is_digit(c: u32) -> bool {
    (u32::from('0')..=u32::from('9')).contains(&c)
}

fn is_hex(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| c.is_ascii_hexdigit())
}

fn is_line_terminator(c: u32) -> bool {
    matches!(c, CR | LF | 0x2028 | 0x2029)
}

fn is_letter(c: u32) -> bool {
    [LL, LM, LO, LT, LU, NL]
        .iter()
        .any(|category| in_ranges(category, c))
}

impl Parser {
    fn peek(&self) -> Option<u32> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u32> {
        self.s.get(self.pos + offset).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(u32::from(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Parsed<()> {
        if self.eat(c) { Ok(()) } else { Err(Fail) }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let len = word.chars().count();
        let matches =
            self.s.len() >= self.pos + len && cps_eq(&self.s[self.pos..self.pos + len], word);
        if matches {
            self.pos += len;
        }
        matches
    }

    /// Length of the line terminator at the cursor.
    fn line_terminator(&self) -> Option<usize> {
        match self.peek()? {
            CR if self.peek_at(1) == Some(LF) => Some(2),
            c if is_line_terminator(c) => Some(1),
            _ => None,
        }
    }

    /// Skips whitespace and comments. An unterminated `/*` is left in place.
    fn sp(&mut self) {
        loop {
            let Some(c) = self.peek() else { return };
            if let Some(len) = self.line_terminator() {
                self.pos += len;
            } else if matches!(c, 0x20 | 0x09 | 0x0B | 0x0C | 0xA0 | 0xFEFF) || in_ranges(ZS, c) {
                self.pos += 1;
            } else if c == u32::from('/') && self.peek_at(1) == Some(u32::from('/')) {
                self.pos += 2;
                while self.peek().is_some() && self.line_terminator().is_none() {
                    self.pos += 1;
                }
            } else if c == u32::from('/') && self.peek_at(1) == Some(u32::from('*')) {
                let body = self.pos + 2;
                let close = self.s[body..]
                    .windows(2)
                    .position(|w| w == [u32::from('*'), u32::from('/')]);
                match close {
                    Some(at) => self.pos = body + at + 2,
                    None => return,
                }
            } else {
                return;
            }
        }
    }

    fn value(&mut self) -> Parsed<Value> {
        let c = self.peek().ok_or(Fail)?;
        match char::from_u32(c) {
            Some('n') if self.eat_word("null") => Ok(Value::Null),
            Some('t') if self.eat_word("true") => Ok(Value::Bool(true)),
            Some('f') if self.eat_word("false") => Ok(Value::Bool(false)),
            Some('{') => self.nested(Parser::object),
            Some('[') => self.nested(Parser::array),
            Some('"' | '\'') => self.string().map(Value::Str),
            _ => self.number(),
        }
    }

    fn nested(&mut self, body: fn(&mut Parser) -> Parsed<Value>) -> Parsed<Value> {
        self.depth += 1;
        if self.depth > NESTING_LIMIT {
            return Err(Fail);
        }
        let value = body(self)?;
        self.depth -= 1;
        Ok(value)
    }

    /// The items of an array or object after its opening bracket, through
    /// the closing one: comma-separated, with an optional trailing comma.
    fn items<T>(
        &mut self,
        close: char,
        mut item: impl FnMut(&mut Parser) -> Parsed<T>,
    ) -> Parsed<Vec<T>> {
        let mut items = Vec::new();
        self.sp();
        if self.eat(close) {
            return Ok(items);
        }
        loop {
            items.push(item(self)?);
            self.sp();
            if self.eat(close) {
                return Ok(items);
            }
            self.expect(',')?;
            self.sp();
            if self.eat(close) {
                return Ok(items);
            }
        }
    }

    fn array(&mut self) -> Parsed<Value> {
        self.expect('[')?;
        self.items(']', Parser::value).map(Value::List)
    }

    fn object(&mut self) -> Parsed<Value> {
        self.expect('{')?;
        let members = self.items('}', |p| {
            let key = match p.peek().and_then(char::from_u32) {
                Some('"' | '\'') => p.string()?,
                _ => p.identifier()?,
            };
            p.sp();
            p.expect(':')?;
            p.sp();
            Ok((key, p.value()?))
        })?;
        let mut pairs: Vec<(Vec<u32>, Value)> = Vec::new();
        let mut index: HashMap<Vec<u32>, usize> = HashMap::new();
        for (key, value) in members {
            match index.entry(key) {
                Entry::Occupied(at) => pairs[*at.get()].1 = value,
                Entry::Vacant(slot) => {
                    pairs.push((slot.key().clone(), value));
                    slot.insert(pairs.len() - 1);
                }
            }
        }
        Ok(Value::Object(pairs))
    }

    fn string(&mut self) -> Parsed<Vec<u32>> {
        let quote = self.peek().ok_or(Fail)?;
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let c = self.peek().ok_or(Fail)?;
            if c == quote {
                self.pos += 1;
                return Ok(out);
            }
            if c == u32::from('\\') {
                self.pos += 1;
                if let Some(len) = self.line_terminator() {
                    self.pos += len;
                } else {
                    out.push(self.escape()?);
                }
            } else if is_line_terminator(c) {
                return Err(Fail);
            } else {
                out.push(c);
                self.pos += 1;
            }
        }
    }

    /// The escape after a backslash that is not a line continuation.
    fn escape(&mut self) -> Parsed<u32> {
        let c = self.peek().ok_or(Fail)?;
        let simple = match char::from_u32(c) {
            Some('b') => Some(0x08),
            Some('f') => Some(0x0C),
            Some('n') => Some(0x0A),
            Some('r') => Some(0x0D),
            Some('t') => Some(0x09),
            Some('v') => Some(0x0B),
            Some('x') => {
                self.pos += 1;
                return self.hex_digits(2);
            }
            Some('u') => {
                self.pos += 1;
                return self.hex_digits(4);
            }
            Some('0') if !self.peek_at(1).is_some_and(is_digit) => Some(0),
            _ if is_digit(c) => return Err(Fail),
            _ => Some(c),
        };
        self.pos += 1;
        simple.ok_or(Fail)
    }

    fn hex_digits(&mut self, n: usize) -> Parsed<u32> {
        let mut value = 0;
        for _ in 0..n {
            let c = self.peek().filter(|&c| is_hex(c)).ok_or(Fail)?;
            value = value * 16 + char::from_u32(c).and_then(|c| c.to_digit(16)).ok_or(Fail)?;
            self.pos += 1;
        }
        Ok(value)
    }

    /// `\uHHHH` at the cursor, consumed.
    fn unicode_escape(&mut self) -> Option<u32> {
        let start = self.pos;
        if self.eat('\\')
            && self.eat('u')
            && let Ok(c) = self.hex_digits(4)
        {
            return Some(c);
        }
        self.pos = start;
        None
    }

    /// An identifier start character at the cursor, consumed.
    fn id_start(&mut self) -> Option<u32> {
        let c = self.peek()?;
        let ascii =
            char::from_u32(c).is_some_and(|c| c.is_ascii_alphabetic() || c == '$' || c == '_');
        if ascii || is_letter(c) {
            self.pos += 1;
            return Some(c);
        }
        self.unicode_escape()
    }

    fn id_continue(&mut self) -> Option<u32> {
        if let Some(c) = self.id_start() {
            return Some(c);
        }
        let c = self.peek()?;
        let continues = is_digit(c)
            || [MN, MC, ND, PC]
                .iter()
                .any(|category| in_ranges(category, c))
            || c == 0x200C
            || c == 0x200D;
        if continues {
            self.pos += 1;
            Some(c)
        } else {
            None
        }
    }

    fn identifier(&mut self) -> Parsed<Vec<u32>> {
        let mut out = vec![self.id_start().ok_or(Fail)?];
        while let Some(c) = self.id_continue() {
            out.push(c);
        }
        Ok(out)
    }

    /// Whether an identifier start character follows, without consuming it.
    fn id_start_follows(&mut self) -> bool {
        let start = self.pos;
        let found = self.id_start().is_some();
        self.pos = start;
        found
    }

    fn number(&mut self) -> Parsed<Value> {
        let negative = if self.eat('-') {
            true
        } else {
            self.eat('+');
            false
        };
        let start = self.pos;
        if let Some(literal) = self.decimal() {
            if !self.id_start_follows() {
                return convert_decimal(&literal).map(|()| Value::Number);
            }
            self.pos = start;
        }
        if self.eat_word("0x") || self.eat_word("0X") {
            let digits = self.s[self.pos..]
                .iter()
                .take_while(|&&c| is_hex(c))
                .count();
            if digits == 0 {
                return Err(Fail);
            }
            self.pos += digits;
            // A sign before hex sends the literal to int() or float(), which
            // reject it.
            return if negative {
                Err(Fail)
            } else {
                Ok(Value::Number)
            };
        }
        if self.eat_word("Infinity") || self.eat_word("NaN") {
            return Ok(Value::Number);
        }
        Err(Fail)
    }

    /// A decimal literal: an integer part with optional fraction and
    /// exponent, or a fraction with optional exponent. Fractions and
    /// exponents may have no digits here; conversion rejects those.
    fn decimal(&mut self) -> Option<String> {
        let start = self.pos;
        let mut literal = String::new();
        let integer = match self.peek() {
            Some(c) if c == u32::from('0') && !self.peek_at(1).is_some_and(is_digit) => 1,
            Some(c) if is_digit(c) && c != u32::from('0') => self.s[self.pos..]
                .iter()
                .take_while(|&&c| is_digit(c))
                .count(),
            _ => 0,
        };
        self.take(integer, &mut literal);
        if self.peek() == Some(u32::from('.')) {
            let digits = self.s[self.pos + 1..]
                .iter()
                .take_while(|&&c| is_digit(c))
                .count();
            self.take(1 + digits, &mut literal);
        } else if integer == 0 {
            self.pos = start;
            return None;
        }
        if matches!(self.peek().and_then(char::from_u32), Some('e' | 'E')) {
            let sign = usize::from(matches!(
                self.peek_at(1).and_then(char::from_u32),
                Some('+' | '-')
            ));
            let digits = self.s[self.pos + 1 + sign..]
                .iter()
                .take_while(|&&c| is_digit(c))
                .count();
            self.take(1 + sign + digits, &mut literal);
        }
        Some(literal)
    }

    fn take(&mut self, n: usize, into: &mut String) {
        into.extend(
            self.s[self.pos..self.pos + n]
                .iter()
                .filter_map(|&c| char::from_u32(c)),
        );
        self.pos += n;
    }
}

/// Whether a decimal literal converts: a fraction or exponent needs digits
/// in the mantissa and in the exponent, and an integer at most 4300 digits.
fn convert_decimal(literal: &str) -> Parsed<()> {
    if literal.contains(['.', 'e', 'E']) {
        let (mantissa, exponent) = match literal.find(['e', 'E']) {
            Some(at) => (&literal[..at], Some(&literal[at + 1..])),
            None => (literal, None),
        };
        let mantissa_ok = mantissa.bytes().any(|b| b.is_ascii_digit());
        let exponent_ok = exponent.is_none_or(|e| e.bytes().any(|b| b.is_ascii_digit()));
        return if mantissa_ok && exponent_ok {
            Ok(())
        } else {
            Err(Fail)
        };
    }
    if literal.len() > MAX_INT_DIGITS {
        Err(Fail)
    } else {
        Ok(())
    }
}
