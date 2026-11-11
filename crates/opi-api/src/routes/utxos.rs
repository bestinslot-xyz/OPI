//! `GET /v1/utxos/wallet`: a wallet's unspent outputs, each with the
//! inscriptions and runes it carries, filtered to the outputs a wallet may
//! spend as plain bitcoin (`type=cardinal`), or to the inscribed or runic
//! ones.
//!
//! The outputs come from the utxos module, their inscriptions from the
//! inscriptions module, their runes from the runes module and, for each
//! inscription, whether it is a used BRC-20 or BRC2.0 transfer inscription
//! from the brc20 module. Each module's rows and height come from one
//! snapshot of its database, and `indexed_height` is the lowest height read:
//! a cardinal answer leaves out the outputs created above it, since a module
//! that has not reached their block would show them clean.
//!
//! The route needs the utxos, inscriptions and runes modules, and the brc20
//! module for `type=cardinal`; without one it answers
//! `404 {"error":"<module> is not enabled on this node","result":null}`.

use {
    super::{
        ModuleRoutes, RouteSettings,
        inscriptions::{
            EntryRow, IndexedHeight, entries_on, indexed_height,
            outputs::{OutputRune, RuneRow, not_enabled, runes_height, runes_on},
            snapshot,
        },
        paging::{BadParam, Wallet, single},
    },
    crate::{
        config::Module,
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, ErrorBody},
        reply::{ApiError, Reply},
    },
    axum::extract::State,
    std::collections::{HashMap, HashSet},
    utoipa::{IntoParams, ToSchema},
};

#[cfg(test)]
mod tests;

/// The most unspent outputs a wallet may hold for the route to list them.
pub const MAX_WALLET_UTXOS: i64 = 10_000;

/// The brc20 event types of a transfer inscription's use: `transfer-transfer`
/// and the BRC2.0 deploy, call and withdraw transfers. An inscription with
/// one has spent its effect, so its output may be spent as plain bitcoin.
const USED_TRANSFER_EVENT_TYPES: [i32; 4] = [3, 5, 7, 9];

/// No route reads the utxos database alone: `/v1/utxos/wallet` reads
/// several modules and is registered with [`Wallets`].
pub fn routes(_db: &Db, _settings: &RouteSettings) -> ModuleRoutes {
    ModuleRoutes::new()
}

/// The databases `/v1/utxos/wallet` reads: the utxos module's, and each of
/// the others' when it is enabled.
#[derive(Clone)]
pub struct Wallets {
    pub utxos: Db,
    pub inscriptions: Option<Db>,
    pub runes: Option<Db>,
    pub brc20: Option<Db>,
}

/// The outputs `type` keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Every unspent output.
    All,
    /// The outputs without runes whose inscriptions, if any, are all used
    /// transfer inscriptions, created at or below the indexed height.
    Cardinal,
    /// The outputs with at least one inscription.
    Inscribed,
    /// The outputs with at least one rune balance.
    Runic,
}

impl Kind {
    pub fn parse(query: &JsObject) -> Result<Kind, BadParam> {
        Ok(match single(query, "type")? {
            None | Some("") | Some("all") => Kind::All,
            Some("cardinal") => Kind::Cardinal,
            Some("inscribed") => Kind::Inscribed,
            Some("runic") => Kind::Runic,
            Some(_) => {
                return Err(BadParam(
                    "type must be all, cardinal, inscribed or runic".into(),
                ));
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::All => "all",
            Kind::Cardinal => "cardinal",
            Kind::Inscribed => "inscribed",
            Kind::Runic => "runic",
        }
    }
}

/// What an output carries, as the filter weighs it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Carries {
    pub inscriptions: usize,
    /// Inscriptions not known to be used transfer inscriptions.
    pub live_inscriptions: usize,
    pub runes: usize,
}

/// Whether `kind` keeps an output carrying `carries`, created at `height`,
/// in an answer covering the chain up to `indexed_height`.
pub fn keeps(kind: Kind, carries: Carries, height: i32, indexed_height: Option<i32>) -> bool {
    match kind {
        Kind::All => true,
        Kind::Inscribed => carries.inscriptions > 0,
        Kind::Runic => carries.runes > 0,
        Kind::Cardinal => {
            carries.runes == 0
                && carries.live_inscriptions == 0
                && indexed_height.is_some_and(|indexed| height <= indexed)
        }
    }
}

/// The wallet and output type of `/v1/utxos/wallet`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct WalletUtxosParams {
    /// The wallet's address.
    #[param(max_length = 128)]
    address: Option<String>,
    /// The wallet's pkScript, hex.
    #[param(max_length = 20000, pattern = "^([0-9a-fA-F]{2})*$")]
    pkscript: Option<String>,
    /// The outputs to list: `all` (the default), `cardinal` (no runes, and
    /// only used BRC-20 or BRC2.0 transfer inscriptions if any), `inscribed`
    /// or `runic`.
    #[param(pattern = "^(all|cardinal|inscribed|runic)?$")]
    r#type: Option<String>,
}

/// An inscription on an output.
#[derive(ToSchema)]
#[allow(dead_code)]
struct UtxoInscription {
    /// `<txid>i<index>`.
    inscription_id: String,
    /// The inscribed sat's offset in the output.
    offset: i64,
    /// Whether it is a BRC-20 or BRC2.0 transfer inscription that was
    /// already used; null when the brc20 module is not enabled.
    #[schema(nullable)]
    used_brc20_transfer: bool,
}

/// An unspent output of the wallet.
#[derive(ToSchema)]
#[allow(dead_code)]
struct WalletUtxo {
    /// `<txid>:<vout>`, lowercase.
    outpoint: String,
    txid: String,
    vout: i32,
    /// In sats.
    value: i64,
    /// The block that created the output.
    height: i32,
    coinbase: bool,
    /// By offset, then sequence number.
    inscriptions: Vec<UtxoInscription>,
    /// In the order the output holds them.
    runes: Vec<OutputRune>,
}

/// Each module's indexed height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct WalletUtxosHeights {
    utxos: IndexedHeight,
    inscriptions: IndexedHeight,
    runes: IndexedHeight,
    /// Null when the brc20 module is not enabled.
    brc20: IndexedHeight,
}

/// A wallet's unspent outputs.
#[derive(ToSchema)]
#[allow(dead_code)]
struct WalletUtxos {
    /// The address asked for; null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// The `type` applied.
    r#type: String,
    /// By value, largest first, then by outpoint.
    utxos: Vec<WalletUtxo>,
    /// The sum of the listed outputs' values, in sats.
    total_value: i64,
    /// The lowest of the modules' heights, null while any has no block.
    indexed_height: IndexedHeight,
    heights: WalletUtxosHeights,
}

/// A `utxos` row.
struct UtxoRow {
    outpoint: String,
    txid: String,
    vout: i32,
    value: i64,
    height: i32,
    coinbase: bool,
}

/// A wallet's unspent outputs, with their inscriptions and runes.
///
/// The outputs of the wallet, by `address` or `pkscript`, each with the
/// inscriptions and rune balances it carries. `type=cardinal` keeps the
/// outputs a wallet may spend as plain bitcoin: no runes, and no
/// inscriptions other than used BRC-20 or BRC2.0 transfer inscriptions,
/// created at or below `indexed_height`. A wallet holding more than 10000
/// unspent outputs answers 413. A wallet the database does not know answers
/// an empty list.
#[utoipa::path(
    get,
    path = "/v1/utxos/wallet",
    tag = "utxos",
    params(WalletUtxosParams),
    responses(
        (status = 200, description = "The outputs", body = Answer<WalletUtxos>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `address` and `pkscript`", body = ErrorBody),
        (status = 404, description = "`inscriptions is not enabled on this node`, `runes is not enabled on this node`, or `brc20 is not enabled on this node` for `type=cardinal`", body = ErrorBody),
        (status = 413, description = "A wallet holding more than 10000 unspent outputs", body = ErrorBody),
    ),
)]
pub async fn wallet(
    State(dbs): State<Wallets>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (inscriptions_db, runes_db) = match (&dbs.inscriptions, &dbs.runes) {
        (Some(inscriptions), Some(runes)) => (inscriptions, runes),
        (None, _) => return Ok(not_enabled(Module::Inscriptions)),
        (_, None) => return Ok(not_enabled(Module::Runes)),
    };
    let (wallet, kind) =
        match Wallet::from_query(&query).and_then(|w| Ok((w, Kind::parse(&query)?))) {
            Ok(request) => request,
            Err(bad) => return Ok(bad.reply()),
        };
    if kind == Kind::Cardinal && dbs.brc20.is_none() {
        return Ok(not_enabled(Module::Brc20));
    }

    let mut tx = snapshot(&dbs.utxos).await?;
    let utxos_height =
        sqlx::query_scalar!(r#"select max(block_height) as "h?" from utxos_block_hashes;"#)
            .fetch_one(&mut *tx)
            .await?;
    let network = sqlx::query_scalar!(
        r#"select network_type as "network_type!" from utxos_network_type limit 1;"#
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_default();
    let rows = match wallet.pkscript_bytes(&network) {
        None => Vec::new(),
        // utxos_pkscript_idx
        Some(script) => {
            sqlx::query_as!(
                UtxoRow,
                r#"select encode(txid, 'hex') || ':' || vout as "outpoint!",
                        encode(txid, 'hex') as "txid!", vout, value, height, coinbase
                    from utxos
                    where pkscript = $1
                    limit $2;"#,
                script,
                MAX_WALLET_UTXOS + 1,
            )
            .fetch_all(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    if rows.len() as i64 > MAX_WALLET_UTXOS {
        return Ok(Reply::error(
            413,
            &format!("the wallet holds more than {MAX_WALLET_UTXOS} unspent outputs"),
        ));
    }
    let outpoints: Vec<String> = rows.iter().map(|row| row.outpoint.clone()).collect();

    let ((inscriptions_height, entries), (runes_height, runes)) = futures::try_join!(
        async {
            let mut tx = snapshot(inscriptions_db).await?;
            let height = indexed_height(&mut tx).await?;
            let rows = entries_on(&mut tx, &outpoints).await?;
            tx.commit().await?;
            Ok::<_, ApiError>((height, rows))
        },
        async {
            let mut tx = snapshot(runes_db).await?;
            let height = runes_height(&mut tx).await?;
            let rows = runes_on(&mut tx, &outpoints).await?;
            tx.commit().await?;
            Ok::<_, ApiError>((height, rows))
        },
    )?;
    let (brc20_height, used) = match &dbs.brc20 {
        None => (None, None),
        Some(db) => {
            let ids: Vec<String> = entries.iter().map(|e| e.inscription_id.clone()).collect();
            let mut tx = snapshot(db).await?;
            let height =
                sqlx::query_scalar!(r#"select max(block_height) as "h?" from brc20_block_hashes;"#)
                    .fetch_one(&mut *tx)
                    .await?;
            // brc20_events_inscription_id_event_type_idx
            let used: HashSet<String> = sqlx::query_scalar!(
                r#"select distinct inscription_id as "inscription_id!"
                    from brc20_events
                    where inscription_id = any($1::text[]) and event_type = any($2::int4[]);"#,
                &ids,
                &USED_TRANSFER_EVENT_TYPES,
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
            tx.commit().await?;
            (height, Some(used))
        }
    };

    let mut heights = vec![utxos_height, inscriptions_height, runes_height];
    if dbs.brc20.is_some() {
        heights.push(brc20_height);
    }
    let indexed_height = heights
        .iter()
        .copied()
        .collect::<Option<Vec<i32>>>()
        .and_then(|heights| heights.into_iter().min());

    let mut inscriptions_of: HashMap<String, Vec<EntryRow>> = HashMap::new();
    for entry in entries {
        inscriptions_of
            .entry(entry.outpoint.clone())
            .or_default()
            .push(entry);
    }
    let mut runes_of: HashMap<String, Vec<RuneRow>> = HashMap::new();
    for row in runes {
        runes_of.entry(row.outpoint.clone()).or_default().push(row);
    }

    let mut kept: Vec<(UtxoRow, Vec<EntryRow>, Vec<RuneRow>)> = Vec::new();
    for row in rows {
        let inscriptions = inscriptions_of.remove(&row.outpoint).unwrap_or_default();
        let runes = runes_of.remove(&row.outpoint).unwrap_or_default();
        let carries = Carries {
            inscriptions: inscriptions.len(),
            live_inscriptions: inscriptions
                .iter()
                .filter(|e| {
                    !used
                        .as_ref()
                        .is_some_and(|used| used.contains(&e.inscription_id))
                })
                .count(),
            runes: runes.len(),
        };
        if keeps(kind, carries, row.height, indexed_height) {
            kept.push((row, inscriptions, runes));
        }
    }
    kept.sort_by(|(a, ..), (b, ..)| {
        b.value
            .cmp(&a.value)
            .then_with(|| a.outpoint.cmp(&b.outpoint))
    });

    let total_value: i64 = kept.iter().map(|(row, ..)| row.value).sum();
    let utxos: Vec<JsValue> = kept
        .into_iter()
        .map(|(row, inscriptions, runes)| {
            let inscriptions: Vec<JsValue> = inscriptions
                .into_iter()
                .map(|entry| {
                    let used = used
                        .as_ref()
                        .map(|used| used.contains(&entry.inscription_id));
                    js_object! {
                        "inscription_id" => entry.inscription_id,
                        "offset" => entry.offset as f64,
                        "used_brc20_transfer" => used,
                    }
                    .into()
                })
                .collect();
            let runes: Vec<JsValue> = runes.into_iter().map(RuneRow::into_json).collect();
            js_object! {
                "outpoint" => row.outpoint,
                "txid" => row.txid,
                "vout" => row.vout,
                "value" => row.value as f64,
                "height" => row.height,
                "coinbase" => row.coinbase,
                "inscriptions" => inscriptions,
                "runes" => runes,
            }
            .into()
        })
        .collect();
    let (address, pkscript) = match &wallet {
        Wallet::Address(address) => (Some(address.clone()), None),
        Wallet::PkScript(pkscript) => (None, Some(pkscript.clone())),
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "address" => address,
            "pkscript" => pkscript,
            "type" => kind.name(),
            "utxos" => utxos,
            "total_value" => total_value as f64,
            "indexed_height" => indexed_height,
            "heights" => js_object! {
                "utxos" => utxos_height,
                "inscriptions" => inscriptions_height,
                "runes" => runes_height,
                "brc20" => brc20_height,
            },
        },
    }))
}
