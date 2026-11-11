//! The mempool as the wallet route reads it: each transaction's spends,
//! outputs, fee and vsize, indexed by spent outpoint and by output script.

use {
    bitcoin::{OutPoint, Transaction, Txid},
    std::collections::{HashMap, HashSet},
};

/// The witness bytes that open an ord inscription envelope: `OP_FALSE OP_IF`
/// and a push of `ord`.
const ENVELOPE: [u8; 6] = [0x00, 0x63, 0x03, b'o', b'r', b'd'];

/// The script prefix of a runestone: `OP_RETURN OP_13`.
const RUNESTONE: [u8; 2] = [0x6a, 0x5d];

/// Most in-mempool ancestors a lineage walk visits; bitcoind's default
/// policy allows 25.
const MAX_LINEAGE: usize = 100;

/// An output of a mempool transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOutput {
    pub vout: u32,
    pub pkscript: Vec<u8>,
    pub value: u64,
}

/// A transaction the view holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MempoolTx {
    pub txid: Txid,
    pub inputs: Vec<OutPoint>,
    pub outputs: Vec<TxOutput>,
    pub fee: u64,
    pub vsize: u64,
    /// The transaction may move or create an inscription or a rune by
    /// itself: an input witness opens an envelope, or an output holds a
    /// runestone.
    pub marked: bool,
    /// The bitcoind tip when the transaction left the mempool; the view keeps
    /// it until the utxos module reaches that height, which covers the block
    /// that confirmed it.
    pub left_at: Option<u32>,
}

impl MempoolTx {
    pub fn new(tx: &Transaction, fee: u64, vsize: u64) -> MempoolTx {
        let marked = tx
            .input
            .iter()
            .flat_map(|input| input.witness.iter())
            .any(|element| element.windows(ENVELOPE.len()).any(|w| w == ENVELOPE))
            || tx
                .output
                .iter()
                .any(|output| output.script_pubkey.as_bytes().starts_with(&RUNESTONE));
        MempoolTx {
            txid: tx.compute_txid(),
            inputs: tx.input.iter().map(|input| input.previous_output).collect(),
            outputs: tx
                .output
                .iter()
                .enumerate()
                .map(|(vout, output)| TxOutput {
                    vout: vout as u32,
                    pkscript: output.script_pubkey.to_bytes(),
                    value: output.value.to_sat(),
                })
                .collect(),
            fee,
            vsize,
            marked,
            left_at: None,
        }
    }
}

/// A wallet's output in the mempool, unspent by the rest of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MempoolOutput {
    pub outpoint: OutPoint,
    pub value: u64,
    pub fee: u64,
    pub vsize: u64,
}

/// What a mempool output's contents depend on: whether its transaction or an
/// in-mempool ancestor is marked, and the confirmed outputs they spend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lineage {
    pub marked: bool,
    pub confirmed_inputs: Vec<OutPoint>,
}

#[derive(Debug, Default)]
pub struct State {
    txs: HashMap<Txid, MempoolTx>,
    spent_by: HashMap<OutPoint, Txid>,
    by_script: HashMap<Vec<u8>, HashSet<OutPoint>>,
}

impl State {
    /// Adds `tx`, replacing a copy the view held.
    pub fn insert(&mut self, tx: MempoolTx) {
        self.remove(&tx.txid);
        for input in &tx.inputs {
            self.spent_by.insert(*input, tx.txid);
        }
        for output in &tx.outputs {
            self.by_script
                .entry(output.pkscript.clone())
                .or_default()
                .insert(OutPoint {
                    txid: tx.txid,
                    vout: output.vout,
                });
        }
        self.txs.insert(tx.txid, tx);
    }

    pub fn remove(&mut self, txid: &Txid) {
        let Some(tx) = self.txs.remove(txid) else {
            return;
        };
        for input in &tx.inputs {
            if self.spent_by.get(input) == Some(txid) {
                self.spent_by.remove(input);
            }
        }
        for output in &tx.outputs {
            if let Some(outpoints) = self.by_script.get_mut(&output.pkscript) {
                outpoints.remove(&OutPoint {
                    txid: *txid,
                    vout: output.vout,
                });
                if outpoints.is_empty() {
                    self.by_script.remove(&output.pkscript);
                }
            }
        }
    }

    /// Marks `txid` as gone from bitcoind's mempool while the tip was `tip`.
    pub fn leave(&mut self, txid: &Txid, tip: u32) {
        if let Some(tx) = self.txs.get_mut(txid) {
            tx.left_at.get_or_insert(tip);
        }
    }

    /// Drops the transactions that left at or below `utxos_height`: the
    /// utxos module holds their block's outputs by now, or they were
    /// replaced.
    pub fn purge(&mut self, utxos_height: u32) {
        let gone: Vec<Txid> = self
            .txs
            .values()
            .filter(|tx| tx.left_at.is_some_and(|left| left <= utxos_height))
            .map(|tx| tx.txid)
            .collect();
        for txid in gone {
            self.remove(&txid);
        }
    }

    /// The transactions still in bitcoind's mempool as the view last read it.
    pub fn live(&self) -> HashSet<Txid> {
        self.txs
            .values()
            .filter(|tx| tx.left_at.is_none())
            .map(|tx| tx.txid)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    /// Whether a mempool transaction spends `outpoint`.
    pub fn is_spent(&self, outpoint: &OutPoint) -> bool {
        self.spent_by.contains_key(outpoint)
    }

    /// The outputs to `pkscript` that no mempool transaction spends.
    pub fn outputs_to(&self, pkscript: &[u8]) -> Vec<MempoolOutput> {
        let Some(outpoints) = self.by_script.get(pkscript) else {
            return Vec::new();
        };
        let mut outputs: Vec<MempoolOutput> = outpoints
            .iter()
            .filter(|outpoint| !self.is_spent(outpoint))
            .filter_map(|outpoint| {
                let tx = self.txs.get(&outpoint.txid)?;
                let output = tx.outputs.iter().find(|o| o.vout == outpoint.vout)?;
                Some(MempoolOutput {
                    outpoint: *outpoint,
                    value: output.value,
                    fee: tx.fee,
                    vsize: tx.vsize,
                })
            })
            .collect();
        outputs.sort_by_key(|output| (output.outpoint.txid, output.outpoint.vout));
        outputs
    }

    /// The lineage of `txid` through its in-mempool ancestors. A walk longer
    /// than `MAX_LINEAGE` counts as marked.
    pub fn lineage(&self, txid: &Txid) -> Lineage {
        let mut lineage = Lineage::default();
        let mut seen = HashSet::new();
        let mut queue = vec![*txid];
        while let Some(txid) = queue.pop() {
            if !seen.insert(txid) {
                continue;
            }
            if seen.len() > MAX_LINEAGE {
                lineage.marked = true;
                break;
            }
            let Some(tx) = self.txs.get(&txid) else {
                continue;
            };
            lineage.marked |= tx.marked;
            for input in &tx.inputs {
                if self.txs.contains_key(&input.txid) {
                    queue.push(input.txid);
                } else {
                    lineage.confirmed_inputs.push(*input);
                }
            }
        }
        lineage.confirmed_inputs.sort();
        lineage.confirmed_inputs.dedup();
        lineage
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{
            Amount, ScriptBuf, Sequence, TxIn, TxOut, Witness, absolute::LockTime, hashes::Hash,
            transaction::Version,
        },
    };

    fn script(tag: u8) -> Vec<u8> {
        let mut bytes = vec![0x00, 0x14];
        bytes.extend([tag; 20]);
        bytes
    }

    fn confirmed(n: u8) -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([n; 32]),
            vout: 0,
        }
    }

    fn tx(inputs: &[OutPoint], outputs: &[(u64, Vec<u8>)], witness: &[&[u8]]) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::from_slice(witness),
                })
                .collect(),
            output: outputs
                .iter()
                .map(|(value, script)| TxOut {
                    value: Amount::from_sat(*value),
                    script_pubkey: ScriptBuf::from_bytes(script.clone()),
                })
                .collect(),
        }
    }

    fn out(tx: &Transaction, vout: u32) -> OutPoint {
        OutPoint {
            txid: tx.compute_txid(),
            vout,
        }
    }

    #[test]
    fn envelopes_and_runestones_mark_a_transaction() {
        let plain = tx(&[confirmed(1)], &[(1, script(1))], &[&[1, 2, 3]]);
        assert!(!MempoolTx::new(&plain, 0, 0).marked);
        let mut envelope = vec![0x20];
        envelope.extend([7; 32]);
        envelope.extend(ENVELOPE);
        envelope.extend([0x01, 0x01, 0x68]);
        let inscribing = tx(&[confirmed(1)], &[(1, script(1))], &[&[0; 64], &envelope]);
        assert!(MempoolTx::new(&inscribing, 0, 0).marked);
        let etching = tx(
            &[confirmed(1)],
            &[(1, script(1)), (0, vec![0x6a, 0x5d, 0x02, 0x00, 0x01])],
            &[],
        );
        assert!(MempoolTx::new(&etching, 0, 0).marked);
    }

    #[test]
    fn outputs_spends_and_removal_are_indexed() {
        let mut state = State::default();
        let parent = tx(&[confirmed(1)], &[(900, script(1)), (50, script(2))], &[]);
        let child = tx(&[out(&parent, 1)], &[(40, script(2))], &[]);
        state.insert(MempoolTx::new(&parent, 100, 150));
        state.insert(MempoolTx::new(&child, 10, 110));

        assert!(state.is_spent(&confirmed(1)));
        assert!(state.is_spent(&out(&parent, 1)));
        // parent:1 is spent by the child, so only the child's output is left.
        assert_eq!(
            state.outputs_to(&script(2)),
            vec![MempoolOutput {
                outpoint: out(&child, 0),
                value: 40,
                fee: 10,
                vsize: 110,
            }]
        );
        assert_eq!(state.outputs_to(&script(1)).len(), 1);

        state.remove(&child.compute_txid());
        assert!(!state.is_spent(&out(&parent, 1)));
        assert_eq!(state.outputs_to(&script(2))[0].outpoint, out(&parent, 1));
        state.remove(&parent.compute_txid());
        assert!(state.is_empty());
        assert!(!state.is_spent(&confirmed(1)));
        assert!(state.outputs_to(&script(1)).is_empty());
    }

    #[test]
    fn a_departed_transaction_stays_until_the_utxos_module_reaches_its_tip() {
        let mut state = State::default();
        let mined = tx(&[confirmed(1)], &[(1, script(1))], &[]);
        state.insert(MempoolTx::new(&mined, 0, 0));
        state.leave(&mined.compute_txid(), 101);
        assert!(state.live().is_empty());
        state.purge(100);
        assert!(
            state.is_spent(&confirmed(1)),
            "kept while utxos is below 101"
        );
        state.purge(101);
        assert!(state.is_empty());
    }

    #[test]
    fn lineage_follows_mempool_parents_to_confirmed_inputs() {
        let mut state = State::default();
        let mut envelope = ENVELOPE.to_vec();
        envelope.push(0x68);
        let grandparent = tx(&[confirmed(1)], &[(10, script(1))], &[&envelope]);
        let parent = tx(
            &[out(&grandparent, 0), confirmed(2)],
            &[(9, script(1))],
            &[],
        );
        let child = tx(&[out(&parent, 0), confirmed(3)], &[(8, script(1))], &[]);
        let other = tx(&[confirmed(4)], &[(7, script(1))], &[]);
        for t in [&grandparent, &parent, &child, &other] {
            state.insert(MempoolTx::new(t, 0, 0));
        }
        let lineage = state.lineage(&child.compute_txid());
        assert!(lineage.marked, "the grandparent inscribes");
        assert_eq!(
            lineage.confirmed_inputs,
            vec![confirmed(1), confirmed(2), confirmed(3)]
        );
        let clean = state.lineage(&other.compute_txid());
        assert!(!clean.marked);
        assert_eq!(clean.confirmed_inputs, vec![confirmed(4)]);
    }
}
