//! `Db::query` against a live Postgres, set by OPI_API_TEST_DATABASE_URL
//! (skipped when unset). The queries select constants only.

use {
    opi_api::{
        db::Db,
        js::{JsValue, json_stringify},
    },
    sqlx::postgres::PgPoolOptions,
};

fn db() -> Option<Db> {
    let Ok(url) = std::env::var("OPI_API_TEST_DATABASE_URL") else {
        eprintln!("OPI_API_TEST_DATABASE_URL is unset; skipping");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&url)
        .expect("a database URL");
    Some(Db::from_pool(pool))
}

fn json(rows: &[opi_api::js::JsObject]) -> String {
    let rows: Vec<JsValue> = rows.iter().cloned().map(JsValue::Object).collect();
    json_stringify(&JsValue::Array(rows)).unwrap()
}

#[tokio::test]
async fn rows_decode_as_node_pg_parses_them() {
    let Some(db) = db() else { return };
    let rows = db
        .query(
            r#"select $1::int4 as a, 2::int8 as b, 1.50::numeric as c,
                      '{"b": 1, "a": [1, 2.50]}'::jsonb as d,
                      array['x', NULL, 'y z']::text[] as e, null::text as f,
                      array[1000, 10]::numeric(40,0)[] as g,
                      '2024-01-02 03:04:05.678+05:30'::timestamptz as t,
                      $2::text as u, count(*) as n, 'v'::varchar(4) as v, 1 as a"#,
            &["7".into(), JsValue::Undefined],
        )
        .await
        .unwrap();
    assert_eq!(
        json(&rows),
        r#"[{"a":1,"b":"2","c":"1.50","d":{"a":[1,2.5],"b":1},"e":["x",null,"y z"],"f":null,"g":[1000,10],"t":"2024-01-01T21:34:05.678Z","u":null,"n":"1","v":"v"}]"#
    );
}

#[tokio::test]
async fn parameters_take_their_type_from_the_query() {
    let Some(db) = db() else { return };
    // an untyped '4' compares as an integer, and limit takes a bigint
    let rows = db
        .query(
            "select 1 as one where 10 > $1 limit $2",
            &["4".into(), 51.0.into()],
        )
        .await
        .unwrap();
    assert_eq!(json(&rows), r#"[{"one":1}]"#);
    // text round-trips with quotes and backslashes
    let text = r#"it's \ "x" $1"#;
    let rows = db.query("select $1 as s", &[text.into()]).await.unwrap();
    assert_eq!(rows[0].get("s"), &JsValue::from(text));
    // a repeated key's array is an array literal, which no integer reads
    let array = JsValue::Array(vec!["5".into(), "6".into()]);
    assert!(db.query("select 1 where 10 > $1", &[array]).await.is_err());
    assert!(
        db.query("select 1 where 10 > $1", &["abc".into()])
            .await
            .is_err()
    );
    // the parameter count must match, as for a prepared statement
    assert!(db.query("select 1", &["1".into()]).await.is_err());
}
