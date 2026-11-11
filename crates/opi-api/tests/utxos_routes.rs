//! `/v1/utxos/wallet` over HTTP.
//!
//! The first tests use a database double: the modules the route needs and
//! its parameters are checked before any query. The seeded test builds a
//! throwaway database from the brc20, inscriptions, runes and utxos schemas
//! on the local Postgres and skips unless `OPI_TEST_PG=1`.

mod common;

use {
    axum::Router,
    common::{check_against_document, get, height, modules_app, with_seeded_modules},
    opi_api::{config::Module, db::Db},
    serde_json::{Value, json},
};

/// A P2WPKH wallet: its script and its mainnet address.
const SCRIPT: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
const ADDRESS: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
/// Another wallet's script.
const OTHER: &str = "0014111111111111111111111111111111111111111111";

fn tx(n: u32) -> String {
    format!("{n:064x}")
}

fn id(n: u32) -> String {
    format!("{}i0", tx(1000 + n))
}

fn fake() -> Db {
    Db::fake(|_| Ok(7.into()))
}

#[tokio::test]
async fn the_route_needs_inscriptions_runes_and_for_cardinal_brc20() {
    let uri = format!("/v1/utxos/wallet?pkscript={SCRIPT}");
    for (modules, missing) in [
        (&[Module::Utxos][..], "inscriptions"),
        (&[Module::Utxos, Module::Inscriptions][..], "runes"),
    ] {
        let answer = get(&modules_app(modules, fake(), &[]), &uri).await;
        assert_eq!(answer.status, 404, "{modules:?}");
        assert_eq!(
            answer.body,
            json!({"error": format!("{missing} is not enabled on this node"), "result": null})
                .to_string()
        );
    }
    let app = modules_app(
        &[Module::Utxos, Module::Inscriptions, Module::Runes],
        fake(),
        &[],
    );
    let answer = get(&app, &format!("{uri}&type=cardinal")).await;
    assert_eq!(answer.status, 404);
    assert!(answer.body.contains("brc20 is not enabled on this node"));
}

#[tokio::test]
async fn invalid_parameters_answer_400_before_any_query() {
    let app = modules_app(
        &[
            Module::Brc20,
            Module::Inscriptions,
            Module::Runes,
            Module::Utxos,
        ],
        fake(),
        &[],
    );
    for (uri, reason) in [
        (
            "/v1/utxos/wallet".to_string(),
            "address or pkscript is required",
        ),
        (
            format!("/v1/utxos/wallet?address={ADDRESS}&pkscript={SCRIPT}"),
            "give address or pkscript, not both",
        ),
        (
            "/v1/utxos/wallet?pkscript=abc".to_string(),
            "pkscript must be hex of at most 20000 digits",
        ),
        (
            format!("/v1/utxos/wallet?pkscript={SCRIPT}&type=ordinal"),
            "type must be all, cardinal, inscribed or runic",
        ),
    ] {
        let answer = get(&app, &uri).await;
        assert_eq!(answer.status, 400, "{uri}");
        assert_eq!(
            answer.body,
            json!({"error": reason, "result": null}).to_string(),
            "{uri}"
        );
        check_against_document(&uri, &answer);
    }
}

#[tokio::test]
async fn without_utxos_the_prefix_is_not_enabled() {
    let answer = get(&height(Module::Runes, 7.into()), "/v1/utxos/wallet").await;
    assert_eq!(answer.status, 404);
    assert!(answer.body.contains("utxos is not enabled on this node"));
}

/// The fixture: five outputs of the wallet and one of another.
///
/// | output  | value | height | carries |
/// | tx1:0   | 1000  | 10     | nothing |
/// | tx2:0   | 546   | 10     | a used BRC-20 transfer inscription |
/// | tx3:0   | 546   | 10     | a BRC-20 transfer inscription not yet used |
/// | tx4:1   | 10000 | 10     | a rune balance |
/// | tx5:0   | 5000  | 20     | nothing, above the inscriptions module's height 15 |
/// | tx6:0   | 7     | 10     | another wallet's |
fn seed() -> String {
    let utxo = |n: u32, vout: u32, value: u64, height: u32, script: &str| {
        format!(
            "insert into utxos values ('\\x{}', {vout}, {value}, {height}, false, '\\x{script}');",
            tx(n)
        )
    };
    let entry = |seq: u32, n: u32| {
        format!(
            "insert into inscriptions_entries (sequence_number, \"number\", \"offset\", last_event_id, \
             charms, vout, genesis_height, last_moved_height, inscription_id, content_type, \
             media_type, txid, pkscript) values ({seq}, {seq}, 0, 0, 0, 0, 10, 10, '{}', \
             'text/plain', 'text/plain', '\\x{}', '\\x{SCRIPT}');",
            id(seq),
            tx(n)
        )
    };
    [
        "insert into utxos_network_type (network_type) values ('mainnet');".to_string(),
        "insert into inscriptions_network_type (network_type) values ('mainnet');".to_string(),
        utxo(1, 0, 1000, 10, SCRIPT),
        utxo(2, 0, 546, 10, SCRIPT),
        utxo(3, 0, 546, 10, SCRIPT),
        utxo(4, 1, 10000, 10, SCRIPT),
        utxo(5, 0, 5000, 20, SCRIPT),
        utxo(6, 0, 7, 10, OTHER),
        "insert into utxos_block_hashes values (20, 'h20');".to_string(),
        entry(1, 2),
        entry(2, 3),
        "insert into inscriptions_block_hashes (block_height, block_hash, block_time) \
         values (15, 'h15', now());"
            .to_string(),
        format!(
            "insert into runes_outpoint_to_balances values ('\\x{}', 1, '\\x{SCRIPT}', \
             array['840000:1'], array[5::numeric], 10, null);",
            tx(4)
        ),
        "insert into runes_block_hashes (block_height, block_hash, block_time) \
         values (20, 'h20', now());"
            .to_string(),
        format!(
            "insert into brc20_events (event_type, block_height, inscription_id, event) values \
             (2, 10, '{a}', '{{}}'), (3, 12, '{a}', '{{}}'), (2, 10, '{b}', '{{}}');",
            a = id(1),
            b = id(2)
        ),
        "insert into brc20_block_hashes (block_height, block_hash) values (20, 'h20');".to_string(),
    ]
    .join("\n")
}

async fn result(app: &Router, uri: &str) -> Value {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, 200, "{uri}: {}", answer.body);
    check_against_document(uri, &answer);
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    body["result"].clone()
}

fn outpoints(result: &Value) -> Vec<String> {
    result["utxos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|utxo| utxo["outpoint"].as_str().unwrap().to_string())
        .collect()
}

fn op(n: u32, vout: u32) -> String {
    format!("{}:{vout}", tx(n))
}

#[tokio::test]
async fn the_wallet_route_classifies_outputs() {
    let seed = seed();
    with_seeded_modules(
        &[
            Module::Brc20,
            Module::Inscriptions,
            Module::Runes,
            Module::Utxos,
        ],
        "utxos_wallet",
        None,
        &[
            include_str!("../../opi-brc20/src/database/sql/db_init.sql"),
            include_str!("../../opi-inscriptions/src/db_init.sql"),
            include_str!("../../opi-runes/src/db_init.sql"),
            include_str!("../../opi-utxos/src/db_init.sql"),
            &seed,
        ],
        4,
        |app, _pool| async move {
            let base = format!("/v1/utxos/wallet?pkscript={SCRIPT}");

            let all = result(&app, &base).await;
            assert_eq!(all["type"], "all");
            assert_eq!(
                outpoints(&all),
                [op(4, 1), op(5, 0), op(1, 0), op(2, 0), op(3, 0)]
            );
            assert_eq!(all["total_value"], 17092);
            assert_eq!(all["indexed_height"], 15);
            assert_eq!(
                all["heights"],
                json!({"utxos": 20, "inscriptions": 15, "runes": 20, "brc20": 20})
            );
            let first = &all["utxos"][0];
            assert_eq!(first["value"], 10000);
            assert_eq!(first["runes"][0]["rune_id"], "840000:1");
            assert_eq!(first["runes"][0]["amount"], "5");
            let used = &all["utxos"][3];
            assert_eq!(
                used["inscriptions"],
                json!([{"inscription_id": id(1), "offset": 0, "used_brc20_transfer": true}])
            );
            assert_eq!(
                all["utxos"][4]["inscriptions"][0]["used_brc20_transfer"],
                false
            );

            // No runes, only used transfers, and not above the lowest height.
            let cardinal = result(&app, &format!("{base}&type=cardinal")).await;
            assert_eq!(outpoints(&cardinal), [op(1, 0), op(2, 0)]);
            assert_eq!(cardinal["total_value"], 1546);

            let inscribed = result(&app, &format!("{base}&type=inscribed")).await;
            assert_eq!(outpoints(&inscribed), [op(2, 0), op(3, 0)]);
            let runic = result(&app, &format!("{base}&type=runic")).await;
            assert_eq!(outpoints(&runic), [op(4, 1)]);

            // An address resolves to the same script.
            let by_address = result(&app, &format!("/v1/utxos/wallet?address={ADDRESS}")).await;
            assert_eq!(outpoints(&by_address), outpoints(&all));
            assert_eq!(by_address["address"], ADDRESS);
            assert_eq!(by_address["pkscript"], Value::Null);

            // An unknown wallet answers an empty list.
            let empty = result(
                &app,
                &format!("/v1/utxos/wallet?pkscript={}", "00".repeat(22)),
            )
            .await;
            assert_eq!(empty["utxos"], json!([]));
            assert_eq!(empty["total_value"], 0);
        },
    )
    .await;
}
