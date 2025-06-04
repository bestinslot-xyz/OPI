use super::*;

#[derive(Debug, PartialEq, Copy, Clone, Default, DeserializeFromStr, SerializeDisplay)]
pub struct Decimal {
  pub value: u128,
  pub scale: u8,
}

impl Decimal {
  pub fn to_integer(self, divisibility: u8) -> Result<u128> {
    match divisibility.checked_sub(self.scale) {
      Some(difference) => Ok(
        self
          .value
          .checked_mul(
            10u128
              .checked_pow(u32::from(difference))
              .context("divisibility out of range")?,
          )
          .context("amount out of range")?,
      ),
      None => bail!("excessive precision"),
    }
  }
}

impl Display for Decimal {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    let magnitude = 10u128.checked_pow(self.scale.into()).ok_or(fmt::Error)?;

    let integer = self.value / magnitude;
    let mut fraction = self.value % magnitude;

    write!(f, "{integer}")?;

    if fraction > 0 {
      let mut width = self.scale.into();

      while fraction % 10 == 0 {
        fraction /= 10;
        width -= 1;
      }

      write!(f, ".{fraction:0>width$}", width = width)?;
    }

    Ok(())
  }
}

impl FromStr for Decimal {
  type Err = Error;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    if let Some((integer, decimal)) = s.split_once('.') {
      if integer.is_empty() && decimal.is_empty() {
        bail!("empty decimal");
      }

      let integer = if integer.is_empty() {
        0
      } else {
        integer.parse::<u128>()?
      };

      let (decimal, scale) = if decimal.is_empty() {
        (0, 0)
      } else {
        let trailing_zeros = decimal.chars().rev().take_while(|c| *c == '0').count();
        let significant_digits = decimal.chars().count() - trailing_zeros;
        let decimal = decimal.parse::<u128>()?
          / 10u128
            .checked_pow(u32::try_from(trailing_zeros).unwrap())
            .context("excessive trailing zeros")?;
        (decimal, u8::try_from(significant_digits).unwrap())
      };

      Ok(Self {
        value: integer * 10u128.pow(u32::from(scale)) + decimal,
        scale,
      })
    } else {
      Ok(Self {
        value: s.parse::<u128>()?,
        scale: 0,
      })
    }
  }
}
