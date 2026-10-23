#![allow(dead_code)]

use {
  bitcoin::{
    Block, Network, OutPoint, ScriptBuf, Txid, Witness, opcodes,
    script::{self, PushBytesBuf},
  },
  mockcore::TransactionTemplate,
  ord::InscriptionId,
};

/// A mock bitcoind plus helpers that build ord inscription transactions on it.
pub struct Harness {
  pub core: mockcore::Handle,
}

impl Harness {
  /// Starts a regtest mock bitcoind.
  pub fn new() -> Harness {
    Harness {
      core: mockcore::builder().network(Network::Regtest).build(),
    }
  }

  /// Mines a funding block, then broadcasts (without mining) a reveal
  /// transaction inscribing `body`. Returns the id of the new inscription
  /// (`<reveal txid>i0`). Call `mine` to confirm it.
  pub fn inscribe(&self, body: &[u8], content_type: &str) -> InscriptionId {
    self.core.mine_blocks(1);
    let funding_height = usize::try_from(self.core.height()).unwrap();
    let txid = self.core.broadcast_tx(TransactionTemplate {
      inputs: &[(
        funding_height,
        0,
        0,
        envelope(content_type.as_bytes(), body),
      )],
      ..Default::default()
    });
    InscriptionId { txid, index: 0 }
  }

  /// Broadcasts (without mining) a transaction spending `outpoint` to a single
  /// new output, paying `fee`. `outpoint` must be confirmed.
  pub fn transfer(&self, outpoint: OutPoint, fee: u64) -> Txid {
    let (block, tx) = self.core.tx_index(outpoint.txid);
    self.core.broadcast_tx(TransactionTemplate {
      fee,
      inputs: &[(block, tx, outpoint.vout as usize, Witness::new())],
      ..Default::default()
    })
  }

  /// Mines `n` blocks, confirming everything in the mempool in the first.
  pub fn mine(&self, n: usize) -> Vec<Block> {
    self.core.mine_blocks(n as u64)
  }
}

/// Tapscript witness `OP_FALSE OP_IF "ord" 1 <content_type> 0 <body> OP_ENDIF`.
pub fn envelope(content_type: &[u8], body: &[u8]) -> Witness {
  let push = |data: &[u8]| {
    let mut buf = PushBytesBuf::new();
    buf.extend_from_slice(data).unwrap();
    buf
  };

  let mut builder = script::Builder::new()
    .push_opcode(opcodes::OP_FALSE)
    .push_opcode(opcodes::all::OP_IF)
    .push_slice(push(b"ord"))
    .push_slice(push(&[1]))
    .push_slice(push(content_type))
    .push_slice(push(&[]));

  for chunk in body.chunks(520) {
    builder = builder.push_slice(push(chunk));
  }

  let script: ScriptBuf = builder.push_opcode(opcodes::all::OP_ENDIF).into_script();

  Witness::from_slice(&[script.into_bytes(), Vec::new()])
}
