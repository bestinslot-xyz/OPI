#![allow(
  clippy::large_enum_variant,
  clippy::result_large_err,
  clippy::too_many_arguments,
  clippy::type_complexity
)]
#![deny(
  clippy::cast_lossless,
  clippy::cast_possible_truncation,
  clippy::cast_possible_wrap,
  clippy::cast_sign_loss
)]

use {
  self::{
    arguments::Arguments,
    decimal::Decimal,
    properties::Properties,
    representation::Representation,
    settings::Settings,
    subcommand::{OutputFormat, Subcommand},
  },
  anyhow::{anyhow, bail, ensure, Context, Error},
  bitcoin::{
    address::{Address, NetworkUnchecked},
    blockdata::constants::MAX_SCRIPT_ELEMENT_SIZE,
    consensus::{self, Decodable, Encodable},
    hash_types::BlockHash,
    hashes::Hash,
    script, Amount, Block, Network, OutPoint, Script, ScriptBuf, SignedAmount, Transaction, TxOut,
    Txid, Witness,
  },
  bitcoincore_rpc::{Client, RpcApi},
  chrono::{DateTime, TimeZone, Utc},
  ciborium::Value,
  clap::{ArgGroup, Parser},
  error::{ResultExt, SnafuError},
  lazy_static::lazy_static,
  ordinals::{Charm, Height, Rune, RuneId, Sat, SatPoint, SpacedRune},
  regex::Regex,
  serde::{Deserialize, Serialize},
  serde_with::{DeserializeFromStr, SerializeDisplay},
  snafu::{Backtrace, ErrorCompat, Snafu},
  std::{
    backtrace::BacktraceStatus,
    collections::{BTreeMap, HashSet},
    env,
    ffi::OsString,
    fmt::{self, Display, Formatter},
    fs::{self, File},
    io::{self, BufReader, Cursor, Read},
    mem,
    path::{Path, PathBuf},
    process::{self},
    str::FromStr,
    sync::{
      atomic::{self, AtomicBool},
      Mutex,
    },
    thread,
    time::{Duration, Instant},
  },
  sysinfo::System,
};

pub use self::{
  chain::Chain,
  fee_rate::FeeRate,
  index::Index,
  inscriptions::{Envelope, Inscription, InscriptionId, ParsedEnvelope, RawEnvelope},
  object::Object,
  options::Options,
};

pub mod arguments;
pub mod chain;
pub mod decimal;
mod error;
mod fee_rate;
pub mod index;
mod inscriptions;
mod object;
pub mod options;
pub mod outgoing;
mod properties;
mod re;
mod representation;
pub mod settings;
pub mod subcommand;

type Result<T = (), E = Error> = std::result::Result<T, E>;
type SnafuResult<T = (), E = SnafuError> = std::result::Result<T, E>;

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static LISTENERS: Mutex<Vec<axum_server::Handle>> = Mutex::new(Vec::new());
static INDEXER: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

#[doc(hidden)]
#[derive(Deserialize, Serialize)]
pub struct SimulateRawTransactionResult {
  #[serde(with = "bitcoin::amount::serde::as_btc")]
  pub balance_change: SignedAmount,
}

#[doc(hidden)]
#[derive(Deserialize, Serialize)]
pub struct SimulateRawTransactionOptions {
  include_watchonly: bool,
}

pub fn timestamp(seconds: u64) -> DateTime<Utc> {
  Utc
    .timestamp_opt(seconds.try_into().unwrap_or(i64::MAX), 0)
    .unwrap()
}

pub fn unbound_outpoint() -> OutPoint {
  OutPoint {
    txid: Hash::all_zeros(),
    vout: 0,
  }
}

pub fn base64_encode(data: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>> {
  use base64::Engine;
  Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

fn default<T: Default>() -> T {
  Default::default()
}

pub fn cancel_shutdown() {
  SHUTTING_DOWN.store(false, atomic::Ordering::Relaxed);
}

pub fn shut_down() {
  SHUTTING_DOWN.store(true, atomic::Ordering::Relaxed);
}

fn gracefully_shut_down_indexer() {
  if let Some(indexer) = INDEXER.lock().unwrap().take() {
    shut_down();
    log::info!("Waiting for index thread to finish...");
    if indexer.join().is_err() {
      log::warn!("Index thread panicked; join failed");
    }
  }
}

/// Nota bene: This function extracts the leaf script from a witness if the
/// witness could represent a taproot script path spend, respecting and
/// ignoring the taproot script annex, if present. Note that the witness may
/// not actually be for a P2TR output, and the leaf script version is ignored.
/// This means that this function will return scripts for any witness program
/// version, past and present, as well as for any leaf script version.
fn unversioned_leaf_script_from_witness(witness: &Witness) -> Option<&Script> {
  #[allow(deprecated)]
  witness.tapscript()
}

pub fn main() {
  env_logger::init();

  ctrlc::set_handler(move || {
    if SHUTTING_DOWN.fetch_or(true, atomic::Ordering::Relaxed) {
      process::exit(1);
    }

    eprintln!("Shutting down gracefully. Press <CTRL-C> again to shutdown immediately.");

    LISTENERS
      .lock()
      .unwrap()
      .iter()
      .for_each(|handle| handle.graceful_shutdown(Some(Duration::from_millis(100))));

    gracefully_shut_down_indexer();
  })
  .expect("Error setting <CTRL-C> handler");

  let args = Arguments::parse();

  let format = args.options.format;

  match args.run() {
    Err(err) => {
      eprintln!("error: {err}");

      if let SnafuError::Anyhow { err } = err {
        for (i, err) in err.chain().skip(1).enumerate() {
          if i == 0 {
            eprintln!();
            eprintln!("because:");
          }

          eprintln!("- {err}");
        }

        if env::var_os("RUST_BACKTRACE")
          .map(|val| val == "1")
          .unwrap_or_default()
        {
          eprintln!("{}", err.backtrace());
        }
      } else {
        for (i, err) in err.iter_chain().skip(1).enumerate() {
          if i == 0 {
            eprintln!();
            eprintln!("because:");
          }

          eprintln!("- {err}");
        }

        if let Some(backtrace) = err.backtrace() {
          if backtrace.status() == BacktraceStatus::Captured {
            eprintln!("backtrace:");
            eprintln!("{backtrace}");
          }
        }
      }

      gracefully_shut_down_indexer();

      process::exit(1);
    }
    Ok(output) => {
      if let Some(output) = output {
        output.print(format.unwrap_or_default());
      }
      gracefully_shut_down_indexer();
    }
  }
}
