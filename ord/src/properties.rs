use {
  super::*,
  minicbor::{decode, encode, Decode, Decoder, Encode, Encoder},
};

#[derive(Debug, Default, PartialEq)]
pub struct Properties {
  pub(crate) gallery: Vec<InscriptionId>,
}

impl Properties {
  pub(crate) fn to_cbor(&self) -> Option<Vec<u8>> {
    if *self == Self::default() {
      return None;
    }

    Some(
      minicbor::to_vec(RawProperties {
        gallery: Some(
          self
            .gallery
            .iter()
            .copied()
            .map(|item| GalleryItem { id: Some(item) })
            .collect(),
        ),
      })
      .unwrap(),
    )
  }
}

#[derive(Decode, Encode)]
#[cbor(map)]
pub(crate) struct GalleryItem {
  #[n(0)]
  pub(crate) id: Option<InscriptionId>,
}

#[derive(Decode, Encode)]
#[cbor(map)]
pub(crate) struct RawProperties {
  #[n(0)]
  pub(crate) gallery: Option<Vec<GalleryItem>>,
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Error)))]
enum DecodeError {
  #[snafu(display("invalid inscription ID length {len}"))]
  InscriptionId { len: usize },
}

impl<'a, T> Decode<'a, T> for InscriptionId {
  fn decode(decoder: &mut Decoder<'a>, _: &mut T) -> Result<Self, decode::Error> {
    let bytes = decoder.bytes()?;

    Self::from_value(bytes)
      .ok_or_else(|| decode::Error::custom(InscriptionIdError { len: bytes.len() }.build()))
  }
}

impl<T> Encode<T> for InscriptionId {
  fn encode<W>(&self, encoder: &mut Encoder<W>, _: &mut T) -> Result<(), encode::Error<W::Error>>
  where
    W: encode::Write,
  {
    encoder.bytes(&self.value()).map(|_| ())
  }
}
