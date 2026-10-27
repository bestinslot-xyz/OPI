//! Responses as Express sends them.
//!
//! - `response.send(string)` sets `text/html; charset=utf-8`.
//! - `response.send(object)` is `response.json(object)`: `JSON.stringify`
//!   with no spacing, as `application/json; charset=utf-8`.
//! - The app's error middleware answers any handler failure with
//!   `500 {"error":"internal error","result":null}`.
//! - finalhandler answers a request no route takes with its 404 page.
//!
//! Express's `X-Powered-By` and `ETag` headers are not sent.

use {
    crate::js::{JsValue, json_stringify},
    axum::{
        http::{HeaderValue, StatusCode, header},
        response::{IntoResponse, Response},
    },
    std::fmt::Write,
};

#[cfg(test)]
mod tests;

pub const HTML: &str = "text/html; charset=utf-8";
pub const JSON: &str = "application/json; charset=utf-8";

/// A route's answer: a status and an Express `send` body.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Reply {
    /// `response.send(text)`.
    pub fn send_str(text: impl Into<String>) -> Reply {
        Reply {
            status: 200,
            content_type: HTML,
            body: text.into(),
        }
    }

    /// `response.send(value)` for an object or array: `JSON.stringify`.
    pub fn send_json(value: impl Into<JsValue>) -> Reply {
        Reply {
            status: 200,
            content_type: JSON,
            body: json_stringify(&value.into()).unwrap_or_default(),
        }
    }

    /// `response.status(status).send({ error: message, result: null })`.
    pub fn error(status: u16, message: &str) -> Reply {
        Reply::send_json(crate::js_object! {
            "error" => message,
            "result" => JsValue::Null,
        })
        .status(status)
    }

    /// `response.status(status)`.
    pub fn status(mut self, status: u16) -> Reply {
        self.status = status;
        self
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        let mut response = self.body.into_response();
        *response.status_mut() =
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.content_type),
        );
        response
    }
}

/// Any failure in a handler: a thrown JavaScript error, a failed query. It
/// answers `500 {"error":"internal error","result":null}` and logs the cause.
#[derive(Debug)]
pub struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        ApiError(error.into())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

/// The 500 answer the app's error middleware sends.
pub fn internal_error() -> Response {
    Reply::error(500, "internal error").into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        internal_error()
    }
}

/// finalhandler's 404 for a request no route takes:
/// `Cannot <METHOD> <path>` in its HTML error page.
pub fn not_found(method: &str, path: &str) -> Response {
    let message = escape_html(&format!("Cannot {method} {}", encode_url(path)));
    let body = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>{message}</pre>\n</body>\n</html>\n"
    );
    let mut response = Reply::send_str(body).status(404).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// encodeURI on one character.
fn encode_uri_char(out: &mut String, c: char) {
    let unescaped = c.is_ascii_alphanumeric()
        || matches!(
            c,
            ';' | ','
                | '/'
                | '?'
                | ':'
                | '@'
                | '&'
                | '='
                | '+'
                | '$'
                | '-'
                | '_'
                | '.'
                | '!'
                | '~'
                | '*'
                | '\''
                | '('
                | ')'
                | '#'
        );
    if unescaped {
        out.push(c);
    } else {
        let mut buf = [0; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            write!(out, "%{b:02X}").expect("writing to a String");
        }
    }
}

/// encodeurl 1.0.2: its regex passes each run of characters that are not
/// URL characters to encodeURI. A `%` starts such a run unless two hex
/// digits follow, and the run takes the characters its pattern
/// `%(?:[^hex]|hex[^hex]|$)` matched, so `%%41` becomes `%25%2541`. A `%`
/// with one hex digit before the end matches nothing and stays.
pub fn encode_url(url: &str) -> String {
    let chars: Vec<char> = url.chars().collect();
    let hex = |i: usize| chars.get(i).is_some_and(char::is_ascii_hexdigit);
    let mut out = String::with_capacity(url.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '%' {
            let taken = match chars.get(i + 1) {
                None => 1,
                Some(_) if !hex(i + 1) => 2,
                Some(_) if i + 2 < chars.len() && !hex(i + 2) => 3,
                // a valid escape, or one hex digit at the end
                Some(_) => 0,
            };
            if taken == 0 {
                out.push('%');
                i += 1;
            } else {
                for &c in &chars[i..i + taken] {
                    encode_uri_char(&mut out, c);
                }
                i += taken;
            }
            continue;
        }
        let url_char = matches!(
            c,
            '!' | '&'..=';' | '=' | '?'..='[' | ']' | '_' | 'a'..='z' | '~'
        );
        if url_char {
            out.push(c);
        } else {
            encode_uri_char(&mut out, c);
        }
        i += 1;
    }
    out
}

/// escape-html 1.0.3.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}
