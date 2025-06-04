use super::*;

#[derive(
  Clone, Copy, Debug, DeserializeFromStr, Eq, Hash, PartialEq, PartialOrd, SerializeDisplay,
)]
pub enum Rarity {
  Common,
  Uncommon,
  Rare,
  Epic,
  Legendary,
  Mythic,
}

impl Rarity {
  pub const ALL: [Rarity; 6] = [
    Rarity::Common,
    Rarity::Uncommon,
    Rarity::Rare,
    Rarity::Epic,
    Rarity::Legendary,
    Rarity::Mythic,
  ];

  pub fn supply(self) -> u64 {
    match self {
      Self::Common => 2_099_999_990_760_000,
      Self::Uncommon => 6_926_535,
      Self::Rare => 3_432,
      Self::Epic => 27,
      Self::Legendary => 5,
      Self::Mythic => 1,
    }
  }
}

impl From<Rarity> for u8 {
  fn from(rarity: Rarity) -> Self {
    rarity as u8
  }
}

impl TryFrom<u8> for Rarity {
  type Error = u8;

  fn try_from(rarity: u8) -> Result<Self, u8> {
    match rarity {
      0 => Ok(Self::Common),
      1 => Ok(Self::Uncommon),
      2 => Ok(Self::Rare),
      3 => Ok(Self::Epic),
      4 => Ok(Self::Legendary),
      5 => Ok(Self::Mythic),
      n => Err(n),
    }
  }
}

impl Display for Rarity {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    write!(
      f,
      "{}",
      match self {
        Self::Common => "common",
        Self::Uncommon => "uncommon",
        Self::Rare => "rare",
        Self::Epic => "epic",
        Self::Legendary => "legendary",
        Self::Mythic => "mythic",
      }
    )
  }
}

impl From<Sat> for Rarity {
  fn from(sat: Sat) -> Self {
    let Degree {
      hour,
      minute,
      second,
      third,
    } = sat.degree();

    if hour == 0 && minute == 0 && second == 0 && third == 0 {
      Self::Mythic
    } else if minute == 0 && second == 0 && third == 0 {
      Self::Legendary
    } else if minute == 0 && third == 0 {
      Self::Epic
    } else if second == 0 && third == 0 {
      Self::Rare
    } else if third == 0 {
      Self::Uncommon
    } else {
      Self::Common
    }
  }
}

impl FromStr for Rarity {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "common" => Ok(Self::Common),
      "uncommon" => Ok(Self::Uncommon),
      "rare" => Ok(Self::Rare),
      "epic" => Ok(Self::Epic),
      "legendary" => Ok(Self::Legendary),
      "mythic" => Ok(Self::Mythic),
      _ => Err(format!("invalid rarity `{s}`")),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rarity() {
    assert_eq!(Sat(0).rarity(), Rarity::Mythic);
    assert_eq!(Sat(1).rarity(), Rarity::Common);

    assert_eq!(Sat(50 * COIN_VALUE - 1).rarity(), Rarity::Common);
    assert_eq!(Sat(50 * COIN_VALUE).rarity(), Rarity::Uncommon);
    assert_eq!(Sat(50 * COIN_VALUE + 1).rarity(), Rarity::Common);

    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(DIFFCHANGE_INTERVAL) - 1).rarity(),
      Rarity::Common
    );
    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(DIFFCHANGE_INTERVAL)).rarity(),
      Rarity::Rare
    );
    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(DIFFCHANGE_INTERVAL) + 1).rarity(),
      Rarity::Common
    );

    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(SUBSIDY_HALVING_INTERVAL) - 1).rarity(),
      Rarity::Common
    );
    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(SUBSIDY_HALVING_INTERVAL)).rarity(),
      Rarity::Epic
    );
    assert_eq!(
      Sat(50 * COIN_VALUE * u64::from(SUBSIDY_HALVING_INTERVAL) + 1).rarity(),
      Rarity::Common
    );

    assert_eq!(Sat(2067187500000000 - 1).rarity(), Rarity::Common);
    assert_eq!(Sat(2067187500000000).rarity(), Rarity::Legendary);
    assert_eq!(Sat(2067187500000000 + 1).rarity(), Rarity::Common);
  }

  #[test]
  fn from_str_and_deserialize_ok() {
    #[track_caller]
    fn case(s: &str, expected: Rarity) {
      let actual = s.parse::<Rarity>().unwrap();
      assert_eq!(actual, expected);
      let round_trip = actual.to_string().parse::<Rarity>().unwrap();
      assert_eq!(round_trip, expected);
      let serialized = serde_json::to_string(&expected).unwrap();
      assert!(serde_json::from_str::<Rarity>(&serialized).is_ok());
    }

    case("common", Rarity::Common);
    case("uncommon", Rarity::Uncommon);
    case("rare", Rarity::Rare);
    case("epic", Rarity::Epic);
    case("legendary", Rarity::Legendary);
    case("mythic", Rarity::Mythic);
  }

  #[test]
  fn conversions_with_u8() {
    for expected in Rarity::ALL {
      let n: u8 = expected.into();
      let actual = Rarity::try_from(n).unwrap();
      assert_eq!(actual, expected);
    }

    assert_eq!(Rarity::try_from(6), Err(6));
  }

  #[test]
  fn error() {
    assert_eq!("foo".parse::<Rarity>().unwrap_err(), "invalid rarity `foo`");
  }

  #[test]
  fn supply() {
    let mut i = 0;

    let mut supply = HashMap::<Rarity, u64>::new();

    for height in 0.. {
      let subsidy = Height(height).subsidy();

      if subsidy == 0 {
        break;
      }

      *supply.entry(Sat(i).rarity()).or_default() += 1;

      *supply.entry(Rarity::Common).or_default() += subsidy.saturating_sub(1);

      i += subsidy;
    }

    for (rarity, supply) in &supply {
      assert_eq!(
        rarity.supply(),
        *supply,
        "invalid supply for rarity {rarity}"
      );
    }

    assert_eq!(supply.values().sum::<u64>(), Sat::SUPPLY);

    assert_eq!(supply.len(), Rarity::ALL.len());
  }
}
