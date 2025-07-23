use super::*;

#[derive(
  Debug, PartialEq, Clone, Copy, derive_more::Display, DeserializeFromStr, SerializeDisplay,
)]
pub struct FeeRate(f64);

impl FromStr for FeeRate {
  type Err = Error;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::try_from(f64::from_str(s)?)
  }
}

impl TryFrom<f64> for FeeRate {
  type Error = Error;

  fn try_from(rate: f64) -> Result<Self, Self::Error> {
    if rate.is_sign_negative() | rate.is_nan() | rate.is_infinite() {
      bail!("invalid fee rate: {rate}")
    }
    Ok(Self(rate))
  }
}

impl FeeRate {
  pub fn fee(&self, vsize: usize) -> Amount {
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    Amount::from_sat((self.0 * vsize as f64).round() as u64)
  }
}
