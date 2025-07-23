use {
  super::*,
  bitcoin::blockdata::{
    opcodes,
    script::{
      Instruction::{self, Op, PushBytes},
      Instructions,
    },
  },
  std::iter::Peekable,
};

pub(crate) const PROTOCOL_ID: [u8; 3] = *b"ord";
pub(crate) const BODY_TAG: [u8; 0] = [];

type Result<T> = std::result::Result<T, script::Error>;
pub type RawEnvelope = Envelope<Vec<Vec<u8>>>;
pub type ParsedEnvelope = Envelope<Inscription>;

#[derive(Default, PartialEq, Clone, Serialize, Deserialize, Debug, Eq)]
pub struct Envelope<T> {
  pub input: u32,
  pub offset: u32,
  pub payload: T,
  pub pushnum: bool,
  pub stutter: bool,
}

impl From<RawEnvelope> for ParsedEnvelope {
  fn from(envelope: RawEnvelope) -> Self {
    let body = envelope
      .payload
      .iter()
      .enumerate()
      .position(|(i, push)| i % 2 == 0 && push.is_empty());

    let mut fields: BTreeMap<&[u8], Vec<&[u8]>> = BTreeMap::new();

    let mut incomplete_field = false;

    for item in envelope.payload[..body.unwrap_or(envelope.payload.len())].chunks(2) {
      match item {
        [key, value] => fields.entry(key).or_default().push(value),
        _ => incomplete_field = true,
      }
    }

    let duplicate_field = fields.iter().any(|(_key, values)| values.len() > 1);

    let content_encoding = Tag::ContentEncoding.take(&mut fields);
    let content_type = Tag::ContentType.take(&mut fields);
    let delegate = Tag::Delegate.take(&mut fields);
    let metadata = Tag::Metadata.take(&mut fields);
    let metaprotocol = Tag::Metaprotocol.take(&mut fields);
    let parents = Tag::Parent.take_array(&mut fields);
    let pointer = Tag::Pointer.take(&mut fields);
    let properties = Tag::Properties.take(&mut fields);
    let rune = Tag::Rune.take(&mut fields);

    let unrecognized_even_field = fields
      .keys()
      .any(|tag| tag.first().map(|lsb| lsb % 2 == 0).unwrap_or_default());

    Self {
      payload: Inscription {
        body: body.map(|i| {
          envelope.payload[i + 1..]
            .iter()
            .flatten()
            .cloned()
            .collect()
        }),
        content_encoding,
        content_type,
        delegate,
        duplicate_field,
        incomplete_field,
        metadata,
        metaprotocol,
        parents,
        pointer,
        properties,
        rune,
        unrecognized_even_field,
      },
      input: envelope.input,
      offset: envelope.offset,
      pushnum: envelope.pushnum,
      stutter: envelope.stutter,
    }
  }
}

impl ParsedEnvelope {
  pub fn from_transaction(transaction: &Transaction) -> Vec<Self> {
    RawEnvelope::from_transaction(transaction)
      .into_iter()
      .map(|envelope| envelope.into())
      .collect()
  }
}

impl RawEnvelope {
  pub fn from_transaction(transaction: &Transaction) -> Vec<Self> {
    let mut envelopes = Vec::new();

    for (i, input) in transaction.input.iter().enumerate() {
      if let Some(tapscript) = unversioned_leaf_script_from_witness(&input.witness) {
        if let Ok(input_envelopes) = Self::from_tapscript(tapscript, i) {
          envelopes.extend(input_envelopes);
        }
      }
    }

    envelopes
  }

  fn from_tapscript(tapscript: &Script, input: usize) -> Result<Vec<Self>> {
    let mut envelopes = Vec::new();

    let mut instructions = tapscript.instructions().peekable();

    let mut stuttered = false;
    while let Some(instruction) = instructions.next().transpose()? {
      if instruction == PushBytes((&[]).into()) {
        let (stutter, envelope) =
          Self::from_instructions(&mut instructions, input, envelopes.len(), stuttered)?;
        if let Some(envelope) = envelope {
          envelopes.push(envelope);
        } else {
          stuttered = stutter;
        }
      }
    }

    Ok(envelopes)
  }

  fn accept(instructions: &mut Peekable<Instructions>, instruction: Instruction) -> Result<bool> {
    if instructions.peek() == Some(&Ok(instruction)) {
      instructions.next().transpose()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn from_instructions(
    instructions: &mut Peekable<Instructions>,
    input: usize,
    offset: usize,
    stutter: bool,
  ) -> Result<(bool, Option<Self>)> {
    if !Self::accept(instructions, Op(opcodes::all::OP_IF))? {
      let stutter = instructions.peek() == Some(&Ok(PushBytes((&[]).into())));
      return Ok((stutter, None));
    }

    if !Self::accept(instructions, PushBytes((&PROTOCOL_ID).into()))? {
      let stutter = instructions.peek() == Some(&Ok(PushBytes((&[]).into())));
      return Ok((stutter, None));
    }

    let mut pushnum = false;

    let mut payload = Vec::new();

    loop {
      match instructions.next().transpose()? {
        None => return Ok((false, None)),
        Some(Op(opcodes::all::OP_ENDIF)) => {
          return Ok((
            false,
            Some(Envelope {
              input: input.try_into().unwrap(),
              offset: offset.try_into().unwrap(),
              payload,
              pushnum,
              stutter,
            }),
          ));
        }
        Some(Op(opcodes::all::OP_PUSHNUM_NEG1)) => {
          pushnum = true;
          payload.push(vec![0x81]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_1)) => {
          pushnum = true;
          payload.push(vec![1]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_2)) => {
          pushnum = true;
          payload.push(vec![2]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_3)) => {
          pushnum = true;
          payload.push(vec![3]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_4)) => {
          pushnum = true;
          payload.push(vec![4]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_5)) => {
          pushnum = true;
          payload.push(vec![5]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_6)) => {
          pushnum = true;
          payload.push(vec![6]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_7)) => {
          pushnum = true;
          payload.push(vec![7]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_8)) => {
          pushnum = true;
          payload.push(vec![8]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_9)) => {
          pushnum = true;
          payload.push(vec![9]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_10)) => {
          pushnum = true;
          payload.push(vec![10]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_11)) => {
          pushnum = true;
          payload.push(vec![11]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_12)) => {
          pushnum = true;
          payload.push(vec![12]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_13)) => {
          pushnum = true;
          payload.push(vec![13]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_14)) => {
          pushnum = true;
          payload.push(vec![14]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_15)) => {
          pushnum = true;
          payload.push(vec![15]);
        }
        Some(Op(opcodes::all::OP_PUSHNUM_16)) => {
          pushnum = true;
          payload.push(vec![16]);
        }
        Some(PushBytes(push)) => {
          payload.push(push.as_bytes().to_vec());
        }
        Some(_) => return Ok((false, None)),
      }
    }
  }
}
