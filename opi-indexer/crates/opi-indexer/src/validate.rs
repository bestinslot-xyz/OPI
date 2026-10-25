//! `opi-indexer validate`: compares the journal with a running `db_reader`
//! block by block.

use {
    crate::{
        config::Config,
        filters::{is_valid_bitmap, is_valid_sns},
    },
    anyhow::{Context, Result, anyhow, bail},
    brc20_index::client::{Brc20Source, JournalBrc20Source},
    clap::Args as ClapArgs,
    db_reader::{BitmapInscription, BlockInfo, Brc20ApiClient, SNSInscription},
    jsonrpsee::http_client::HttpClientBuilder,
    opi_core::{BlockSource, InscriptionRecord},
    opi_source::journal::{Journal, JournalReader},
    serde::Serialize,
    serde_json::{Number, Value},
    std::{fmt, sync::Arc},
};

/// Outpoint `db_reader` reports for an inscription that was sent as fee and
/// lost; upstream ord reports the position inside the lost-sats range.
const LOST_OUTPOINT: &str =
    "0000000000000000000000000000000000000000000000000000000000000000:4294967295";

#[derive(ClapArgs)]
pub struct Args {
    /// JSON-RPC URL of the `db_reader` to compare against.
    #[arg(long)]
    pub db_reader_url: String,
    /// First height to compare.
    #[arg(long)]
    pub from: u32,
    /// Last height to compare, inclusive.
    #[arg(long)]
    pub to: u32,
}

/// Everything compared at one height, one JSON value per method.
pub struct Sides {
    pub brc20: Value,
    pub bitmap: Value,
    pub sns: Value,
    pub header: Value,
}

/// The first section of a height where the journal and `db_reader` differ.
#[derive(Debug)]
pub struct Divergence {
    pub height: u32,
    pub section: &'static str,
    pub journal: Value,
    pub remote: Value,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "DIVERGENCE at height {} in {}",
            self.height, self.section
        )?;
        writeln!(f, "journal:\n{}", pretty(&self.journal))?;
        write!(f, "db_reader:\n{}", pretty(&self.remote))
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Compares the two sides of `height`, returning the first section that
/// differs.
pub fn compare_block(height: u32, journal: &Sides, remote: &Sides) -> Result<(), Box<Divergence>> {
    let sections: [(&'static str, Value, &Value, &Value); 4] = [
        (
            "getBlockBRC20Txes",
            accept_lost_satpoints(&journal.brc20, &remote.brc20),
            &journal.brc20,
            &remote.brc20,
        ),
        (
            "getBlockBitmapInscrs",
            journal.bitmap.clone(),
            &journal.bitmap,
            &remote.bitmap,
        ),
        (
            "getBlockSNSInscrs",
            journal.sns.clone(),
            &journal.sns,
            &remote.sns,
        ),
        (
            "getBlockHashAndTs",
            journal.header.clone(),
            &journal.header,
            &remote.header,
        ),
    ];
    for (section, adjusted, journal, remote) in sections {
        if canonical_numbers(adjusted) != canonical_numbers(remote.clone()) {
            return Err(Box::new(Divergence {
                height,
                section,
                journal: journal.clone(),
                remote: remote.clone(),
            }));
        }
    }
    Ok(())
}

/// Returns `value` with every number in the form both sides share: integers
/// that fit `i64` or `u64` as they are, any other number through `f64`. A
/// number with no finite `f64` stays as it is. `db_reader` re-serializes
/// numbers through `f64` (`1.50` becomes `1.5`) while the journal keeps the
/// literal text.
fn canonical_numbers(mut value: Value) -> Value {
    canonicalize(&mut value);
    value
}

fn canonicalize(value: &mut Value) {
    match value {
        Value::Number(number) => {
            if number.as_i64().is_some() || number.as_u64().is_some() {
                return;
            }
            if let Some(float) = number.as_f64().filter(|float| float.is_finite())
                && let Some(canonical) = Number::from_f64(float)
            {
                *number = canonical;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize),
        Value::Object(map) => map.values_mut().for_each(canonicalize),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// Returns `journal` with `new_satpoint` taken from `remote` for every entry
/// that both sides report as sent as fee and placed at the lost outpoint.
fn accept_lost_satpoints(journal: &Value, remote: &Value) -> Value {
    let (Value::Array(journal_txes), Value::Array(remote_txes)) = (journal, remote) else {
        return journal.clone();
    };
    let adjusted = journal_txes
        .iter()
        .enumerate()
        .map(|(index, journal_tx)| {
            let mut journal_tx = journal_tx.clone();
            if let Some(remote_tx) = remote_txes.get(index)
                && is_lost(remote_tx)
                && is_lost(&journal_tx)
                && let Some(satpoint) = remote_tx.get("new_satpoint")
                && let Some(object) = journal_tx.as_object_mut()
            {
                object.insert("new_satpoint".into(), satpoint.clone());
            }
            journal_tx
        })
        .collect();
    Value::Array(adjusted)
}

fn is_lost(tx: &Value) -> bool {
    tx.get("sent_as_fee") == Some(&Value::Bool(true))
        && tx
            .get("new_satpoint")
            .and_then(Value::as_str)
            .is_some_and(|satpoint| {
                satpoint
                    .strip_prefix(LOST_OUTPOINT)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
            })
}

/// Maps a journal record's info to the shape `db_reader`'s filters take.
fn api_info(info: &opi_core::InscriptionInfo) -> db_reader::InscriptionInfo {
    db_reader::InscriptionInfo {
        _inscription_id: info.inscription_id.clone(),
        inscription_number: info.inscription_number,
        cursed_for_brc20: info.cursed_for_brc20,
        parent_id: info.parent_id.clone(),
        is_json: info.is_json,
        content_hex: hex::encode(&info.content),
        content_type_hex: hex::encode(&info.content_type),
        _metaprotocol_hex: hex::encode(&info.metaprotocol),
    }
}

/// `getBlockBitmapInscrs` over journal records in idx order: creations that
/// pass the Bitmap filter, sorted by inscription number.
pub fn shape_bitmap(
    entries: &[(InscriptionRecord, opi_core::InscriptionInfo)],
) -> Vec<BitmapInscription> {
    let mut inscriptions = Vec::new();
    for (record, info) in entries {
        if record.old_satpoint.is_some() {
            continue;
        }
        let info = api_info(info);
        if !is_valid_bitmap(&info) {
            continue;
        }
        inscriptions.push(BitmapInscription {
            tx_id: format!("{}:{}", record.height, record.idx),
            inscription_id: record.inscription_id.clone(),
            inscription_number: info.inscription_number,
            txid: record.txid.clone(),
            content_hex: info.content_hex,
        });
    }
    inscriptions.sort_by_key(|inscription| inscription.inscription_number);
    inscriptions
}

/// `getBlockSNSInscrs` over journal records in idx order: creations that pass
/// the SNS filter, sorted by inscription number.
pub fn shape_sns(
    entries: &[(InscriptionRecord, opi_core::InscriptionInfo)],
) -> Vec<SNSInscription> {
    let mut inscriptions = Vec::new();
    for (record, info) in entries {
        if record.old_satpoint.is_some() {
            continue;
        }
        let info = api_info(info);
        if !is_valid_sns(&info) {
            continue;
        }
        inscriptions.push(SNSInscription {
            tx_id: format!("{}:{}", record.height, record.idx),
            inscription_id: record.inscription_id.clone(),
            inscription_number: info.inscription_number,
            txid: record.txid.clone(),
            content_hex: info.content_hex,
            content_type_hex: info.content_type_hex,
        });
    }
    inscriptions.sort_by_key(|inscription| inscription.inscription_number);
    inscriptions
}

fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

/// Whether `err` is redb refusing to open a journal another process holds.
fn is_locked(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<redb::DatabaseError>(),
            Some(redb::DatabaseError::DatabaseAlreadyOpen)
        )
    })
}

fn open_journal(config: &Config) -> Result<JournalReader> {
    let path = config.source.journal_path();
    if !path.exists() {
        bail!("no journal at {}", path.display());
    }
    match Journal::open(&path, config.source.chain) {
        Ok(journal) => Ok(JournalReader::new(journal)),
        Err(err) if is_locked(&err) => bail!(
            "journal {} is in use by another process: stop opi-indexer before validating",
            path.display()
        ),
        Err(err) => Err(err),
    }
}

async fn journal_sides(
    reader: &Arc<JournalReader>,
    brc20: &JournalBrc20Source,
    height: u32,
) -> Result<Sides> {
    let transfers = brc20
        .get_transfers(height as i32)
        .await
        .map_err(|err| anyhow!("brc20 transfers at {height}: {err}"))?;

    let mut entries = Vec::new();
    for record in reader.inscription_records(height)? {
        let info = reader
            .inscription_info(&record.inscription_id)?
            .with_context(|| format!("inscription info missing for {}", record.inscription_id))?;
        entries.push((record, info));
    }

    let header = reader.block_info(height)?.map(|info| BlockInfo {
        block_hash: info.hash,
        timestamp: info.time as u64,
    });

    Ok(Sides {
        brc20: to_value(&transfers)?,
        bitmap: to_value(&shape_bitmap(&entries))?,
        sns: to_value(&shape_sns(&entries))?,
        header: to_value(&header)?,
    })
}

async fn remote_sides(client: &impl Brc20ApiClient, height: u32) -> Result<Sides> {
    Ok(Sides {
        brc20: to_value(&client.get_block_brc20_txes(height).await?)?,
        bitmap: to_value(&client.get_block_bitmap_inscrs(height).await?)?,
        sns: to_value(&client.get_block_sns_inscrs(height).await?)?,
        header: to_value(&client.get_block_hash_and_ts(height).await?)?,
    })
}

/// Compares every height in `args.from..=args.to`. Returns whether all of
/// them match; the first divergence is printed.
pub fn run(args: Args, config: &Config) -> Result<bool> {
    if args.from > args.to {
        bail!("--from {} is above --to {}", args.from, args.to);
    }
    let reader = Arc::new(open_journal(config)?);
    match reader.tip()? {
        Some(tip) if args.to <= tip => {}
        tip => bail!(
            "--to {} is above the journal tip ({})",
            args.to,
            tip.map_or("empty".to_string(), |tip| tip.to_string())
        ),
    }
    let source: Arc<dyn BlockSource> = reader.clone();
    let brc20 = JournalBrc20Source::new(source, config.network());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let client = HttpClientBuilder::default().build(&args.db_reader_url)?;
        for height in args.from..=args.to {
            let journal = journal_sides(&reader, &brc20, height).await?;
            let remote = remote_sides(&client, height)
                .await
                .with_context(|| format!("db_reader at height {height}"))?;
            if let Err(divergence) = compare_block(height, &journal, &remote) {
                println!("{divergence}");
                return Ok(false);
            }
            if height % 1000 == 0 {
                tracing::info!("validated up to {height}");
            }
        }
        println!("OK {}..{}", args.from, args.to);
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    fn sides(brc20: Value) -> Sides {
        Sides {
            brc20,
            bitmap: json!([]),
            sns: json!([]),
            header: json!({"block_hash": "aa", "timestamp": 1}),
        }
    }

    fn tx(new_satpoint: &str, sent_as_fee: bool) -> Value {
        json!({
            "tx_id": "100:0",
            "inscription_id": "abi0",
            "new_satpoint": new_satpoint,
            "sent_as_fee": sent_as_fee,
        })
    }

    #[test]
    fn lost_satpoint_difference_is_allowed() {
        let lost = format!("{LOST_OUTPOINT}:7");
        let journal_lost = format!("{LOST_OUTPOINT}:123");
        let remote = sides(json!([tx(&lost, true)]));
        let journal = sides(json!([tx(&journal_lost, true)]));
        assert!(compare_block(100, &journal, &remote).is_ok());

        // Not sent as fee: the same difference is reported.
        let remote = sides(json!([tx(&lost, false)]));
        let journal = sides(json!([tx(&journal_lost, false)]));
        assert!(compare_block(100, &journal, &remote).is_err());

        // Sent as fee but the remote is not at the lost outpoint: reported.
        let remote = sides(json!([tx("ee:1:0", true)]));
        let journal = sides(json!([tx(&journal_lost, true)]));
        assert!(compare_block(100, &journal, &remote).is_err());

        // The remote is lost but the journal is at a normal outpoint: reported.
        let remote = sides(json!([tx(&lost, true)]));
        let journal = sides(json!([tx("ff:0:123", true)]));
        assert!(compare_block(100, &journal, &remote).is_err());
    }

    #[test]
    fn number_formatting_is_not_a_divergence() {
        let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
        // The binary parses with `arbitrary_precision`, so the literal text
        // survives on the journal side.
        assert_eq!(parse(r#"{"amt":1.50}"#).to_string(), r#"{"amt":1.50}"#);

        let journal = sides(parse(r#"[{"amt":1.50,"n":1E2,"i":7}]"#));
        let remote = sides(parse(r#"[{"amt":1.5,"n":100.0,"i":7}]"#));
        assert!(compare_block(100, &journal, &remote).is_ok());

        let journal = sides(parse(r#"[{"amt":2}]"#));
        let remote = sides(parse(r#"[{"amt":3}]"#));
        assert!(compare_block(100, &journal, &remote).is_err());

        // A number with no finite f64 keeps its text, so it still diverges.
        let journal = sides(parse(r#"[{"amt":1e400}]"#));
        let remote = sides(parse(r#"[{"amt":2e400}]"#));
        assert!(compare_block(100, &journal, &remote).is_err());
    }

    #[test]
    fn any_other_difference_is_reported_with_height() {
        let remote = sides(json!([tx("ee:1:0", false)]));
        let journal = sides(json!([tx("ee:1:1", false)]));
        let divergence = compare_block(4242, &journal, &remote).unwrap_err();
        assert_eq!(divergence.height, 4242);
        assert_eq!(divergence.section, "getBlockBRC20Txes");
        assert_eq!(divergence.journal[0]["new_satpoint"], "ee:1:1");
        assert_eq!(divergence.remote[0]["new_satpoint"], "ee:1:0");
        let text = divergence.to_string();
        assert!(text.contains("4242") && text.contains("ee:1:1") && text.contains("ee:1:0"));

        let mut remote = sides(json!([]));
        let journal = sides(json!([]));
        remote.header = json!({"block_hash": "bb", "timestamp": 1});
        let divergence = compare_block(7, &journal, &remote).unwrap_err();
        assert_eq!(divergence.section, "getBlockHashAndTs");

        let mut remote = sides(json!([]));
        remote.brc20 = Value::Null;
        assert!(compare_block(7, &sides(json!([])), &remote).is_err());
    }

    fn entry(
        idx: u32,
        number: i32,
        old_satpoint: Option<&str>,
        content_type: &str,
        is_json: bool,
    ) -> (InscriptionRecord, opi_core::InscriptionInfo) {
        let inscription_id = format!("{:064x}i0", idx);
        (
            InscriptionRecord {
                height: 800_000,
                idx,
                inscription_id: inscription_id.clone(),
                old_satpoint: old_satpoint.map(str::to_string),
                new_satpoint: "aa:0:0".into(),
                sent_as_fee: false,
                new_output_value: 546,
                txid: format!("{:064x}", idx),
                new_pkscript: vec![],
            },
            opi_core::InscriptionInfo {
                inscription_id,
                inscription_number: number,
                cursed_for_brc20: false,
                parent_id: None,
                is_json,
                content: b"1.bitmap".to_vec(),
                content_type: content_type.as_bytes().to_vec(),
                metaprotocol: vec![],
            },
        )
    }

    #[test]
    fn bitmap_shape_sorts_by_inscription_number() {
        let entries = vec![
            entry(0, 30, None, "text/plain", false),
            entry(1, 10, None, "text/plain;charset=utf-8", false),
            entry(2, 20, Some("bb:0:0"), "text/plain", false),
            entry(3, 5, None, "image/png", false),
            entry(4, -1, None, "text/plain", false),
            entry(5, 7, None, "text/plain", true),
            entry(6, 20, None, "text/plain", false),
        ];
        let bitmap = shape_bitmap(&entries);
        let numbers: Vec<i32> = bitmap.iter().map(|b| b.inscription_number).collect();
        assert_eq!(numbers, vec![10, 20, 30]);
        assert_eq!(bitmap[0].tx_id, "800000:1");
        assert_eq!(bitmap[0].txid, format!("{:064x}", 1));
        assert_eq!(bitmap[0].content_hex, hex::encode("1.bitmap"));

        // SNS accepts JSON content and keeps the content type.
        let entries = vec![
            entry(0, 3, None, "application/json", true),
            entry(1, 1, None, "text/plain", false),
            entry(2, 2, None, "image/png", false),
        ];
        let sns = shape_sns(&entries);
        assert_eq!(
            sns.iter().map(|s| s.inscription_number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(sns[1].content_type_hex, hex::encode("application/json"));
    }

    #[test]
    fn locked_journal_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.redb");
        let _held = Journal::open(&path, ord::Chain::Regtest).unwrap();
        let err = Journal::open(&path, ord::Chain::Regtest).err().unwrap();
        assert!(is_locked(&err), "{err:#}");
        assert!(!is_locked(&anyhow!("something else")));
    }
}
