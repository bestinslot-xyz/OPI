//! What a run of consecutive blocks does to the UTXO set.

use bitcoin::{Block, OutPoint, Txid, hashes::Hash};
use std::collections::HashMap;

/// bitcoind's `MAX_SCRIPT_SIZE`: a longer output script can never be spent.
const MAX_SCRIPT_SIZE: usize = 10_000;

/// A txid as the 32 bytes its hex spells, the order the other OPI modules
/// store txids in.
pub fn txid_bytes(txid: &Txid) -> [u8; 32] {
    let mut bytes = txid.to_byte_array();
    bytes.reverse();
    bytes
}

/// An unspent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub txid: [u8; 32],
    pub vout: u32,
    pub pkscript: Vec<u8>,
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
}

/// An outpoint from before the run that a block of the run spends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spend {
    pub txid: [u8; 32],
    pub vout: u32,
    pub height: u32,
}

/// The net change of a run of consecutive blocks: the outputs the run
/// creates and leaves unspent, and the outputs from before the run that it
/// spends. An output created and spent inside the run appears in neither.
#[derive(Debug, Default)]
pub struct Delta {
    created: HashMap<OutPoint, Output>,
    spent: Vec<Spend>,
}

impl Delta {
    pub fn new() -> Delta {
        Delta::default()
    }

    /// Folds in the block at `height`, the block after the last one added.
    /// The genesis block adds nothing: bitcoind never adds its coinbase to
    /// the UTXO set. Unspendable outputs (an OP_RETURN script, or one longer
    /// than `MAX_SCRIPT_SIZE`) are left out, as bitcoind leaves them out. A
    /// coinbase whose txid repeats an earlier one (the two BIP30 exceptions)
    /// replaces the earlier outputs, as it does in bitcoind.
    pub fn add_block(&mut self, height: u32, block: &Block) {
        if height == 0 {
            return;
        }
        for tx in &block.txdata {
            let coinbase = tx.is_coinbase();
            if !coinbase {
                for input in &tx.input {
                    let previous = input.previous_output;
                    if self.created.remove(&previous).is_none() {
                        self.spent.push(Spend {
                            txid: txid_bytes(&previous.txid),
                            vout: previous.vout,
                            height,
                        });
                    }
                }
            }
            let txid = tx.compute_txid();
            for (vout, output) in tx.output.iter().enumerate() {
                let script = output.script_pubkey.as_bytes();
                if output.script_pubkey.is_op_return() || script.len() > MAX_SCRIPT_SIZE {
                    continue;
                }
                let vout = vout as u32;
                self.created.insert(
                    OutPoint { txid, vout },
                    Output {
                        txid: txid_bytes(&txid),
                        vout,
                        pkscript: script.to_vec(),
                        value: output.value.to_sat(),
                        height,
                        coinbase,
                    },
                );
            }
        }
    }

    /// Outputs and spends folded in so far.
    pub fn len(&self) -> usize {
        self.created.len() + self.spent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The created outputs in outpoint order, and the spends in block order.
    pub fn into_parts(self) -> (Vec<Output>, Vec<Spend>) {
        let mut created: Vec<Output> = self.created.into_values().collect();
        created.sort_by_key(|o| (o.txid, o.vout));
        (created, self.spent)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bitcoin::{
        Amount, Network, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
        absolute::LockTime, blockdata::constants::genesis_block, opcodes, script::Builder,
        transaction::Version,
    };

    pub(crate) fn coinbase(tag: u8, outputs: &[(u64, ScriptBuf)]) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: Builder::new().push_slice([tag]).into_script(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: outputs
                .iter()
                .map(|(value, script)| TxOut {
                    value: Amount::from_sat(*value),
                    script_pubkey: script.clone(),
                })
                .collect(),
        }
    }

    pub(crate) fn spend(inputs: &[OutPoint], outputs: &[(u64, ScriptBuf)]) -> Transaction {
        Transaction {
            input: inputs
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect(),
            ..coinbase(0, outputs)
        }
    }

    pub(crate) fn block(txdata: Vec<Transaction>) -> Block {
        Block {
            header: genesis_block(Network::Regtest).header,
            txdata,
        }
    }

    pub(crate) fn script(tag: u8) -> ScriptBuf {
        Builder::new()
            .push_opcode(opcodes::all::OP_PUSHNUM_1)
            .push_slice([tag; 32])
            .into_script()
    }

    fn outpoint(tx: &Transaction, vout: u32) -> OutPoint {
        OutPoint {
            txid: tx.compute_txid(),
            vout,
        }
    }

    #[test]
    fn txid_bytes_spell_the_hex() {
        let tx = coinbase(1, &[(50, script(1))]);
        let txid = tx.compute_txid();
        let hex: String = txid_bytes(&txid)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, txid.to_string());
    }

    #[test]
    fn genesis_adds_nothing() {
        let mut delta = Delta::new();
        delta.add_block(0, &block(vec![coinbase(1, &[(50, script(1))])]));
        assert!(delta.is_empty());
    }

    #[test]
    fn outputs_and_spends_of_one_block() {
        let earlier = OutPoint {
            txid: coinbase(9, &[(1, script(9))]).compute_txid(),
            vout: 0,
        };
        let cb = coinbase(1, &[(50, script(1))]);
        let tx = spend(&[earlier], &[(30, script(2)), (19, script(3))]);
        let mut delta = Delta::new();
        delta.add_block(5, &block(vec![cb.clone(), tx.clone()]));
        let (created, spent) = delta.into_parts();

        assert_eq!(created.len(), 3);
        let cb_out = created
            .iter()
            .find(|o| o.txid == txid_bytes(&cb.compute_txid()))
            .unwrap();
        assert_eq!(
            (cb_out.vout, cb_out.value, cb_out.height, cb_out.coinbase),
            (0, 50, 5, true)
        );
        assert_eq!(cb_out.pkscript, script(1).to_bytes());
        let change = created
            .iter()
            .find(|o| o.txid == txid_bytes(&tx.compute_txid()) && o.vout == 1)
            .unwrap();
        assert_eq!((change.value, change.coinbase), (19, false));
        assert_eq!(
            spent,
            vec![Spend {
                txid: txid_bytes(&earlier.txid),
                vout: 0,
                height: 5
            }]
        );
    }

    #[test]
    fn an_output_spent_inside_the_run_cancels() {
        let cb = coinbase(1, &[(50, script(1))]);
        let first = spend(&[outpoint(&cb, 0)], &[(49, script(2))]);
        let second = spend(&[outpoint(&first, 0)], &[(48, script(3))]);
        let mut delta = Delta::new();
        // Spent in the same block.
        delta.add_block(1, &block(vec![cb.clone(), first.clone()]));
        // Spent in the next block of the run.
        delta.add_block(2, &block(vec![coinbase(2, &[]), second.clone()]));
        let (created, spent) = delta.into_parts();
        assert!(spent.is_empty());
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].txid, txid_bytes(&second.compute_txid()));
        assert_eq!(created[0].height, 2);
    }

    #[test]
    fn unspendable_outputs_are_left_out() {
        let op_return = Builder::new()
            .push_opcode(opcodes::all::OP_RETURN)
            .push_slice([1u8; 4])
            .into_script();
        let too_long = ScriptBuf::from_bytes(vec![0x51; MAX_SCRIPT_SIZE + 1]);
        let longest = ScriptBuf::from_bytes(vec![0x51; MAX_SCRIPT_SIZE]);
        let cb = coinbase(
            1,
            &[
                (0, op_return),
                (1, too_long),
                (2, longest.clone()),
                (3, ScriptBuf::new()),
            ],
        );
        let mut delta = Delta::new();
        delta.add_block(1, &block(vec![cb]));
        let (created, _) = delta.into_parts();
        let kept: Vec<(u32, u64)> = created.iter().map(|o| (o.vout, o.value)).collect();
        assert_eq!(kept, vec![(2, 2), (3, 3)]);
    }

    #[test]
    fn a_repeated_coinbase_replaces_the_earlier_outputs() {
        let cb = coinbase(7, &[(50, script(1))]);
        let mut delta = Delta::new();
        delta.add_block(1, &block(vec![cb.clone()]));
        delta.add_block(2, &block(vec![cb.clone()]));
        let (created, spent) = delta.into_parts();
        assert!(spent.is_empty());
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].height, 2);
    }
}
