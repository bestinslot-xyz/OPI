use super::*;

#[derive(Debug, PartialEq, Clone, DeserializeFromStr, SerializeDisplay)]
pub enum Object {
  Address(Address<NetworkUnchecked>),
  Hash([u8; 32]),
  InscriptionId(InscriptionId),
  Integer(u128),
  OutPoint(OutPoint),
  Rune(SpacedRune),
  Sat(Sat),
  SatPoint(SatPoint),
}

impl FromStr for Object {
  type Err = SnafuError;

  fn from_str(input: &str) -> Result<Self, Self::Err> {
    use Representation::*;

    match input.parse::<Representation>()? {
      Address => Ok(Self::Address(
        input.parse().snafu_context(error::AddressParse { input })?,
      )),
      Decimal | Degree | Percentile | Name => Ok(Self::Sat(
        input.parse().snafu_context(error::SatParse { input })?,
      )),
      Hash => Ok(Self::Hash(
        bitcoin::hashes::sha256::Hash::from_str(input)
          .snafu_context(error::HashParse { input })?
          .to_byte_array(),
      )),
      InscriptionId => Ok(Self::InscriptionId(
        input
          .parse()
          .snafu_context(error::InscriptionIdParse { input })?,
      )),
      Integer => Ok(Self::Integer(
        input.parse().snafu_context(error::IntegerParse { input })?,
      )),
      OutPoint => Ok(Self::OutPoint(
        input
          .parse()
          .snafu_context(error::OutPointParse { input })?,
      )),
      Rune => Ok(Self::Rune(
        input.parse().snafu_context(error::RuneParse { input })?,
      )),
      SatPoint => Ok(Self::SatPoint(
        input
          .parse()
          .snafu_context(error::SatPointParse { input })?,
      )),
    }
  }
}

impl Display for Object {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    match self {
      Self::Address(address) => write!(f, "{}", address.clone().assume_checked()),
      Self::Hash(hash) => {
        for byte in hash {
          write!(f, "{byte:02x}")?;
        }
        Ok(())
      }
      Self::InscriptionId(inscription_id) => write!(f, "{inscription_id}"),
      Self::Integer(integer) => write!(f, "{integer}"),
      Self::OutPoint(outpoint) => write!(f, "{outpoint}"),
      Self::Rune(rune) => write!(f, "{rune}"),
      Self::Sat(sat) => write!(f, "{sat}"),
      Self::SatPoint(satpoint) => write!(f, "{satpoint}"),
    }
  }
}
