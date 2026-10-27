//! The typed result path against a live Postgres, set by
//! OPI_API_TEST_DATABASE_URL (skipped when unset). The queries read only.

use {
    opi_api::{
        config::Module,
        db::Db,
        js::{JsValue, json_stringify},
        params,
        pg_json::{ToJs, oid, text},
    },
    sqlx::{PgPool, Row, postgres::PgPoolOptions},
};

fn pool(var: &str) -> Option<PgPool> {
    let Ok(url) = std::env::var(var) else {
        eprintln!("{var} is unset; skipping");
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&url)
            .expect("a database URL"),
    )
}

#[tokio::test]
async fn typed_values_match_the_text_node_pg_reads() {
    let Some(pool) = pool("OPI_API_TEST_DATABASE_URL") else {
        return;
    };
    // each value decoded from binary, and the same value's text as
    // node-pg parses it
    let row = sqlx::query(
        "select 0.1::float4 as f4, 0.1::float4::text as f4_text,
                1e-7::float8 as f8, 1e-7::float8::text as f8_text,
                9007199254740993::int8 as i8, 7::int2 as i2,
                array['x', null]::text[] as arr",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let f4: f32 = row.get("f4");
    assert_eq!(
        f4.to_js(),
        text(700, Some(row.get::<&str, _>("f4_text"))).unwrap()
    );
    let f8: f64 = row.get("f8");
    assert_eq!(
        f8.to_js(),
        text(701, Some(row.get::<&str, _>("f8_text"))).unwrap()
    );
    assert_eq!(
        row.get::<i64, _>("i8").to_js(),
        JsValue::from("9007199254740993")
    );
    assert_eq!(row.get::<i16, _>("i2").to_js(), JsValue::from(7));
    assert_eq!(
        json_stringify(&row.get::<Vec<Option<String>>, _>("arr").to_js()).unwrap(),
        r#"["x",null]"#
    );
}

#[tokio::test]
async fn text_casts_keep_what_binary_loses() {
    let Some(pool) = pool("OPI_API_TEST_DATABASE_URL") else {
        return;
    };
    let row = sqlx::query(
        r#"select 1.50::numeric::text as n, 'NaN'::numeric::text as nan,
                  '{"b": 1, "aa": [1, 2.50]}'::jsonb::text as j,
                  array[1000, 10]::numeric(40,0)[]::text as na,
                  '2024-01-02 03:04:05.678+05:30'::timestamptz::text as t"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let js = |o: u32, column: &str| json_stringify(&text(o, row.get(column)).unwrap()).unwrap();
    assert_eq!(js(oid::NUMERIC, "n"), r#""1.50""#);
    assert_eq!(js(oid::NUMERIC, "nan"), r#""NaN""#);
    // jsonb keeps Postgres's key order (shorter keys first)
    assert_eq!(js(oid::JSONB, "j"), r#"{"b":1,"aa":[1,2.5]}"#);
    assert_eq!(js(oid::NUMERIC_ARRAY, "na"), "[1000,10]");
    // the session runs in UTC; the instant is the same
    assert_eq!(js(oid::TIMESTAMPTZ, "t"), r#""2024-01-01T21:34:05.678Z""#);
}

#[tokio::test]
async fn params_bind_as_the_type_postgres_infers() {
    let Some(pool) = pool("OPI_API_TEST_DATABASE_URL") else {
        return;
    };
    let rules = Db::from_pool(pool.clone()).int_rules().await.unwrap();
    let height = params::int4(rules, &" 4 ".into()).unwrap();
    let rows = sqlx::query("select 1 as one where 10 > $1")
        .bind(height)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    // a missing param binds NULL, which compares to nothing
    let rows = sqlx::query("select 1 as one where 10 > $1")
        .bind(params::int4(rules, &JsValue::Undefined).unwrap())
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
async fn int_rules_answer_as_the_servers_own_int4in() {
    let Some(pool) = pool("OPI_API_TEST_DATABASE_URL") else {
        return;
    };
    let rules = Db::from_pool(pool.clone()).int_rules().await.unwrap();
    let version: String = sqlx::query_scalar("show server_version_num")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        rules,
        params::IntRules::for_server_version(version.parse().unwrap())
    );
    for input in [
        "5",
        " +5 ",
        "-2147483648",
        "2147483648",
        "1.5",
        "",
        "0x10",
        "0o17",
        "0b101",
        "1_000",
        "_1",
        "1_",
        "0x_1f",
        "0x",
        "+_1",
        "007",
    ] {
        let server: Result<i32, _> = sqlx::query_scalar("select $1::text::int4")
            .bind(input)
            .fetch_one(&pool)
            .await;
        let ours = params::int4(rules, &input.into());
        match (server, ours) {
            (Ok(a), Ok(Some(b))) => assert_eq!(a, b, "{input:?}"),
            (Err(_), Err(_)) => {}
            (server, ours) => panic!("{input:?}: server {server:?}, ours {:?}", ours.ok()),
        }
    }
}

#[tokio::test]
async fn latest_block_height_reads_each_module() {
    for (module, var) in [
        (Module::Brc20, "OPI_API_TEST_BRC20_URL"),
        (Module::Bitmap, "OPI_API_TEST_BITMAP_URL"),
        (Module::Sns, "OPI_API_TEST_SNS_URL"),
        (Module::Runes, "OPI_API_TEST_RUNES_URL"),
    ] {
        let Some(pool) = pool(var) else { continue };
        let height = Db::from_pool(pool)
            .latest_block_height(module)
            .await
            .unwrap();
        assert!(
            matches!(height, JsValue::Number(n) if n > 0.0),
            "{height:?}"
        );
    }
}
