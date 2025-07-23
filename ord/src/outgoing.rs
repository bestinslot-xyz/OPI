use super::*;

#[derive(Debug, PartialEq, Clone, DeserializeFromStr, SerializeDisplay)]
pub enum Outgoing {
  Amount(Amount),
  InscriptionId(InscriptionId),
  Rune { decimal: Decimal, rune: SpacedRune },
  Sat(Sat),
  SatPoint(SatPoint),
}

impl Display for Outgoing {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    match self {
      Self::Amount(amount) => write!(f, "{}", amount.to_string().to_lowercase()),
      Self::InscriptionId(inscription_id) => inscription_id.fmt(f),
      Self::Rune { decimal, rune } => write!(f, "{decimal}:{rune}"),
      Self::Sat(sat) => write!(f, "{}", sat.name()),
      Self::SatPoint(satpoint) => satpoint.fmt(f),
    }
  }
}

impl FromStr for Outgoing {
  type Err = SnafuError;

  fn from_str(input: &str) -> Result<Self, Self::Err> {
    lazy_static! {
      static ref AMOUNT: Regex = Regex::new(
        r"(?x)
        ^
        (
          \d+
          |
          \.\d+
          |
          \d+\.\d+
        )
        \ ?
        (bit|btc|cbtc|mbtc|msat|nbtc|pbtc|sat|satoshi|ubtc)
        (s)?
        $
        "
      )
      .unwrap();
      static ref RUNE: Regex = Regex::new(
        r"(?x)
        ^
        (
          \d+
          |
          \.\d+
          |
          \d+\.\d+
        )
        \s*:\s*
        (
          [A-Z•.]+
        )
        $
        "
      )
      .unwrap();
    }

    if re::SAT_NAME.is_match(input) {
      Ok(Outgoing::Sat(
        input.parse().snafu_context(error::SatParse { input })?,
      ))
    } else if re::SATPOINT.is_match(input) {
      Ok(Outgoing::SatPoint(
        input
          .parse()
          .snafu_context(error::SatPointParse { input })?,
      ))
    } else if re::INSCRIPTION_ID.is_match(input) {
      Ok(Outgoing::InscriptionId(
        input
          .parse()
          .snafu_context(error::InscriptionIdParse { input })?,
      ))
    } else if AMOUNT.is_match(input) {
      Ok(Outgoing::Amount(
        input.parse().snafu_context(error::AmountParse { input })?,
      ))
    } else if let Some(captures) = RUNE.captures(input) {
      let decimal = captures[1]
        .parse::<Decimal>()
        .snafu_context(error::RuneAmountParse { input })?;
      let rune = captures[2]
        .parse()
        .snafu_context(error::RuneParse { input })?;
      Ok(Self::Rune { decimal, rune })
    } else {
      Err(SnafuError::OutgoingParse {
        input: input.to_string(),
      })
    }
  }
}
