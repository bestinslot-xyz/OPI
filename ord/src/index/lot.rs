use {
  super::*,
  std::{
    cmp::{PartialEq, PartialOrd},
    ops::{Add, AddAssign, Div, Rem, Sub, SubAssign},
  },
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct Lot(pub u128);

impl Lot {
  fn checked_add(self, rhs: Self) -> Option<Self> {
    Some(Self(self.0.checked_add(rhs.0)?))
  }

  fn checked_sub(self, rhs: Self) -> Option<Self> {
    Some(Self(self.0.checked_sub(rhs.0)?))
  }
}

impl TryFrom<Lot> for usize {
  type Error = <usize as TryFrom<u128>>::Error;
  fn try_from(lot: Lot) -> Result<Self, Self::Error> {
    usize::try_from(lot.0)
  }
}

impl Add for Lot {
  type Output = Self;
  fn add(self, other: Self) -> Self::Output {
    self.checked_add(other).expect("lot overflow")
  }
}

impl AddAssign for Lot {
  fn add_assign(&mut self, other: Self) {
    *self = *self + other;
  }
}

impl Add<u128> for Lot {
  type Output = Self;
  fn add(self, other: u128) -> Self::Output {
    self + Lot(other)
  }
}

impl AddAssign<u128> for Lot {
  fn add_assign(&mut self, other: u128) {
    *self += Lot(other);
  }
}

impl Sub for Lot {
  type Output = Self;
  fn sub(self, other: Self) -> Self::Output {
    self.checked_sub(other).expect("lot underflow")
  }
}

impl SubAssign for Lot {
  fn sub_assign(&mut self, other: Self) {
    *self = *self - other;
  }
}

impl Div<u128> for Lot {
  type Output = Self;
  fn div(self, other: u128) -> Self::Output {
    Lot(self.0 / other)
  }
}

impl Rem<u128> for Lot {
  type Output = Self;
  fn rem(self, other: u128) -> Self::Output {
    Lot(self.0 % other)
  }
}

impl PartialEq<u128> for Lot {
  fn eq(&self, other: &u128) -> bool {
    self.0 == *other
  }
}

impl PartialOrd<u128> for Lot {
  fn partial_cmp(&self, other: &u128) -> Option<std::cmp::Ordering> {
    self.0.partial_cmp(other)
  }
}
