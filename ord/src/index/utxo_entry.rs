use {ordinals::varint, ref_cast::RefCast, std::ops::Deref};

enum Sats {
  Value(u64),
}

/// A `UtxoEntry` stores the following information about an unspent transaction
/// output, depending on the indexing options:
///
/// If `--index-sats`, the full list of sat ranges, stored as a varint followed
/// by that many 11-byte sat range entries, otherwise the total output value
/// stored as a varint.
///
/// If `--index-addresses`, the script pubkey stored as a varint followed by
/// that many bytes of data.
///
/// If `--index-inscriptions`, the list of inscriptions stored as
/// `(sequence_number, offset)`, with the sequence number stored as a u32 and
/// the offset as a varint.
///
/// Note that the list of inscriptions doesn't need an explicit length, it
/// continues until the end of the array.
///
/// A `UtxoEntry` is the read-only value stored in redb as a byte string. A
/// `UtxoEntryBuf` is the writeable version, used for constructing new
/// `UtxoEntry`s. A `ParsedUtxoEntry` is the parsed value.
#[derive(Debug, RefCast)]
#[repr(transparent)]
pub struct UtxoEntry {
  bytes: [u8],
}

impl UtxoEntry {
  pub fn parse(&self) -> ParsedUtxoEntry {
    let sats;

    let mut offset = 0;
    let (value, varint_len) = varint::decode(&self.bytes).unwrap();
    sats = Sats::Value(value.try_into().unwrap());
    offset += varint_len;

    ParsedUtxoEntry {
      sats,
      inscriptions: Some(&self.bytes[offset..self.bytes.len()]),
    }
  }
}

pub struct ParsedUtxoEntry<'a> {
  sats: Sats,
  inscriptions: Option<&'a [u8]>,
}

impl<'a> ParsedUtxoEntry<'a> {
  pub fn total_value(&self) -> u64 {
    match self.sats {
      Sats::Value(value) => value,
    }
  }

  pub fn parse_inscriptions(&self) -> Vec<(u32, u64)> {
    let inscriptions = self.inscriptions.unwrap();
    let mut byte_offset = 0;
    let mut parsed_inscriptions = Vec::new();

    while byte_offset < inscriptions.len() {
      let sequence_number = u32::from_be_bytes(
        inscriptions[byte_offset..byte_offset + 4]
          .try_into()
          .unwrap(),
      );
      byte_offset += 4;

      let (satpoint_offset, varint_len) = varint::decode(&inscriptions[byte_offset..]).unwrap();
      let satpoint_offset = u64::try_from(satpoint_offset).unwrap();
      byte_offset += varint_len;

      parsed_inscriptions.push((sequence_number, satpoint_offset));
    }

    parsed_inscriptions
  }
}

#[derive(Debug)]
pub struct UtxoEntryBuf {
  pub vec: Vec<u8>,
}

impl UtxoEntryBuf {
  pub fn new() -> Self {
    Self { vec: Vec::new() }
  }

  pub fn new_with_values(vec: Vec<u8>) -> Self {
    Self { vec }
  }

  pub fn push_value(&mut self, value: u64) {
    varint::encode_to_vec(value.into(), &mut self.vec);
  }

  pub fn push_inscription(&mut self, sequence_number: u32, satpoint_offset: u64) {
    self.vec.extend(sequence_number.to_be_bytes());
    varint::encode_to_vec(satpoint_offset.into(), &mut self.vec);
  }

  pub fn empty() -> Self {
    let mut utxo_entry = Self::new();

    utxo_entry.push_value(0);

    utxo_entry
  }

  pub fn as_ref(&self) -> &UtxoEntry {
    UtxoEntry::ref_cast(&self.vec)
  }
}

impl Default for UtxoEntryBuf {
  fn default() -> Self {
    Self::new()
  }
}

impl Deref for UtxoEntryBuf {
  type Target = UtxoEntry;

  fn deref(&self) -> &UtxoEntry {
    self.as_ref()
  }
}
