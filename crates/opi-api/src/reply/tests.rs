//! Content types and bodies the Express app sent (see the task report for
//! the captured responses).

use {
    super::*,
    crate::{js::JsValue, js_object},
    axum::{body::to_bytes, response::IntoResponse},
};

async fn parts(response: axum::response::Response) -> (u16, Option<String>, String) {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn send_str_is_html_utf8() {
    // response.send(block_height + '')
    assert_eq!(
        parts(Reply::send_str("null").into_response()).await,
        (200, Some(HTML.to_string()), "null".to_string())
    );
}

#[tokio::test]
async fn send_json_is_json_stringify() {
    // response.send({ error: null, result: ... })
    let body = js_object! {
        "error" => JsValue::Null,
        "result" => js_object! { "b" => "x", "1" => 1.5, "u" => JsValue::Undefined },
    };
    assert_eq!(
        parts(Reply::send_json(body).into_response()).await,
        (
            200,
            Some(JSON.to_string()),
            r#"{"error":null,"result":{"1":1.5,"b":"x"}}"#.to_string()
        )
    );
}

#[tokio::test]
async fn status_and_error_shape() {
    // response.status(400).send({ error: 'block not indexed yet', result: null })
    assert_eq!(
        parts(Reply::error(400, "block not indexed yet").into_response()).await,
        (
            400,
            Some(JSON.to_string()),
            r#"{"error":"block not indexed yet","result":null}"#.to_string()
        )
    );
    assert_eq!(
        parts(Reply::send_str("x").status(404).into_response())
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn any_handler_error_is_the_500_body() {
    let error = ApiError::from(anyhow::anyhow!("relation does not exist"));
    assert_eq!(
        parts(error.into_response()).await,
        (
            500,
            Some(JSON.to_string()),
            r#"{"error":"internal error","result":null}"#.to_string()
        )
    );
}

#[tokio::test]
async fn not_found_is_finalhandlers_page() {
    let response = not_found("GET", "/a%zz%41b%7B");
    assert_eq!(
        response.headers()["content-security-policy"],
        "default-src 'none'"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        parts(response).await,
        (
            404,
            Some(HTML.to_string()),
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET /a%25zz%41b%7B</pre>\n</body>\n</html>\n".to_string()
        )
    );
}

#[test]
fn encode_url_encodes_a_bad_escape_with_the_character_it_takes() {
    // encodeurl's regex takes the character after a bad `%` into the run it
    // passes to encodeURI, so a second `%` there is encoded too.
    for (url, expected) in [
        ("/%%41", "/%25%2541"),
        ("/%4%41", "/%254%2541"),
        ("/%4", "/%4"),
        ("/%", "/%25"),
        ("/%zz", "/%25zz"),
        ("/%%", "/%25%25"),
        ("/%4z/", "/%254z/"),
        ("/a%41%", "/a%41%25"),
    ] {
        assert_eq!(encode_url(url), expected, "{url}");
    }
}

#[test]
fn encode_url_and_escape_html_match_express() {
    assert_eq!(
        escape_html(&encode_url("/v1/nope/<a>\"q'&$#é%")),
        "/v1/nope/%3Ca%3E%22q&#39;&amp;$#%C3%A9%25"
    );
}
