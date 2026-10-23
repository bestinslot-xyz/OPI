/// One ord event for a transfer, located within its block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    /// Position of the event in the block's ord event stream.
    pub event_index: usize,
    /// Index of the transaction within the block.
    pub tx_index: usize,
    /// Whether the inscription was sent as fee.
    pub fee: bool,
    /// Whether the event is tracked by the BRC-20 indexer.
    pub tracked: bool,
}

/// Returns `(event_index, idx)` for every tracked event, in idx order.
///
/// Within each transaction, non-fee events take the next index in event
/// order, and fee-bound events reserve one index each after them. Only
/// tracked events are reported, but untracked fee-bound events still
/// reserve their index.
pub fn assign_idx(events: &[Located]) -> Vec<(usize, u32)> {
    let mut sorted = events.to_vec();
    sorted.sort_unstable_by_key(|e| (e.tx_index, e.event_index));

    let mut idx = 0u32;
    let mut out = Vec::new();
    for tx in sorted.chunk_by(|a, b| a.tx_index == b.tx_index) {
        for e in tx.iter().filter(|e| !e.fee) {
            if e.tracked {
                idx += 1;
                out.push((e.event_index, idx));
            }
        }
        for e in tx.iter().filter(|e| e.fee) {
            idx += 1;
            if e.tracked {
                out.push((e.event_index, idx));
            }
        }
    }
    out.sort_unstable_by_key(|&(_, idx)| idx);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(event_index: usize, tx_index: usize, fee: bool, tracked: bool) -> Located {
        Located {
            event_index,
            tx_index,
            fee,
            tracked,
        }
    }

    #[test]
    fn located_untracked_leaves_no_gap() {
        let events = [
            loc(0, 1, false, true),
            loc(1, 1, false, false),
            loc(2, 1, false, true),
        ];
        assert_eq!(assign_idx(&events), vec![(0, 1), (2, 2)]);
    }

    #[test]
    fn fee_bound_untracked_leaves_gap() {
        let events = [loc(0, 1, true, false), loc(1, 2, false, true)];
        assert_eq!(assign_idx(&events), vec![(1, 2)]);
    }

    #[test]
    fn fee_bound_takes_its_movers_slot_not_coinbase_slot() {
        // Tx 1 sends A to fee and tx 2 moves B. A's event is emitted last,
        // but its slot comes from tx 1, so A is numbered before B.
        let events = [loc(5, 1, true, true), loc(3, 2, false, true)];
        assert_eq!(assign_idx(&events), vec![(5, 1), (3, 2)]);
    }

    #[test]
    fn located_before_fee_within_one_tx() {
        let events = [loc(0, 1, true, true), loc(1, 1, false, true)];
        assert_eq!(assign_idx(&events), vec![(1, 1), (0, 2)]);
    }

    #[test]
    fn idx_starts_at_one() {
        let events = [loc(7, 3, false, true)];
        assert_eq!(assign_idx(&events), vec![(7, 1)]);
    }

    #[test]
    fn input_order_is_irrelevant() {
        let sorted = [
            loc(0, 1, true, true),
            loc(1, 1, false, true),
            loc(2, 2, false, true),
        ];
        let shuffled = [sorted[2], sorted[0], sorted[1]];
        assert_eq!(assign_idx(&shuffled), assign_idx(&sorted));
    }
}
