use super::*;

pub(crate) trait Entry: Sized {
  type Value;

  fn load(value: Self::Value) -> Self;

  fn store(self) -> Self::Value;
}

pub(super) type HeaderValue = [u8; 80];

impl Entry for Header {
  type Value = HeaderValue;

  fn load(value: Self::Value) -> Self {
    consensus::encode::deserialize(&value).unwrap()
  }

  fn store(self) -> Self::Value {
    let mut buffer = [0; 80];
    let len = self
      .consensus_encode(&mut buffer.as_mut_slice())
      .expect("in-memory writers don't error");
    debug_assert_eq!(len, buffer.len());
    buffer
  }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct InscriptionEntry {
  pub charms: u16,
  pub id: InscriptionId,
  pub inscription_number: i32,
  pub sequence_number: u32,
  pub is_json_or_text: bool,
  pub txcnt_limit: i16,
}

pub(crate) type InscriptionEntryValue = Vec<u8>;

impl Entry for InscriptionEntry {
  type Value = InscriptionEntryValue;

  #[rustfmt::skip]
  fn load(
    data: InscriptionEntryValue,
  ) -> Self {
    let charms = u16::from_be_bytes(data[0..2].try_into().unwrap());
    let id = InscriptionId::load(data[2..38].try_into().unwrap());
    let inscription_number = i32::from_be_bytes(data[38..42].try_into().unwrap());
    let sequence_number = u32::from_be_bytes(data[42..46].try_into().unwrap());
    let is_json_or_text = data[46] != 0;
    let txcnt_limit = i16::from_be_bytes(data[47..49].try_into().unwrap());
    Self {
      charms,
      id,
      inscription_number,
      sequence_number,
      is_json_or_text,
      txcnt_limit,
    }
  }

  fn store(self) -> Self::Value {
    let mut data = Vec::with_capacity(50);
    data.extend(self.charms.to_be_bytes());
    data.extend(self.id.store());
    data.extend(self.inscription_number.to_be_bytes());
    data.extend(self.sequence_number.to_be_bytes());
    data.push(if self.is_json_or_text { 1 } else { 0 });
    data.extend(self.txcnt_limit.to_be_bytes());
    data
  }
}

pub(crate) type InscriptionIdValue = Vec<u8>;

impl Entry for InscriptionId {
  type Value = InscriptionIdValue;

  fn load(value: Self::Value) -> Self {
    Self {
      txid: Txid::from_byte_array(value[0..32].try_into().unwrap()),
      index: u32::from_be_bytes(value[32..36].try_into().unwrap()),
    }
  }

  fn store(self) -> Self::Value {
    let mut value = Vec::with_capacity(36);
    value.extend(self.txid.to_byte_array());
    value.extend(self.index.to_be_bytes());
    value
  }
}

pub(super) type OutPointValue = [u8; 36];

impl Entry for OutPoint {
  type Value = OutPointValue;

  fn load(value: Self::Value) -> Self {
    Decodable::consensus_decode(&mut bitcoin::io::Cursor::new(value)).unwrap()
  }

  fn store(self) -> Self::Value {
    let mut value = [0; 36];
    self.consensus_encode(&mut value.as_mut_slice()).unwrap();
    value
  }
}

pub(super) type SatPointValue = [u8; 44];

impl Entry for SatPoint {
  type Value = SatPointValue;

  fn load(value: Self::Value) -> Self {
    Decodable::consensus_decode(&mut bitcoin::io::Cursor::new(value)).unwrap()
  }

  fn store(self) -> Self::Value {
    let mut value = [0; 44];
    self.consensus_encode(&mut value.as_mut_slice()).unwrap();
    value
  }
}

pub(super) type SatRange = (u64, u64);

impl Entry for SatRange {
  type Value = [u8; 11];

  fn load([b0, b1, b2, b3, b4, b5, b6, b7, b8, b9, b10]: Self::Value) -> Self {
    let raw_base = u64::from_be_bytes([b0, b1, b2, b3, b4, b5, b6, 0]);

    // 51 bit base
    let base = raw_base & ((1 << 51) - 1);

    let raw_delta = u64::from_be_bytes([b6, b7, b8, b9, b10, 0, 0, 0]);

    // 33 bit delta
    let delta = raw_delta >> 3;

    (base, base + delta)
  }

  fn store(self) -> Self::Value {
    let base = self.0;
    let delta = self.1 - self.0;
    let n = u128::from(base) | (u128::from(delta) << 51);
    n.to_be_bytes()[0..11].try_into().unwrap()
  }
}

pub(super) type TxidValue = [u8; 32];

impl Entry for Txid {
  type Value = TxidValue;

  fn load(value: Self::Value) -> Self {
    Txid::from_byte_array(value)
  }

  fn store(self) -> Self::Value {
    Txid::to_byte_array(self)
  }
}
