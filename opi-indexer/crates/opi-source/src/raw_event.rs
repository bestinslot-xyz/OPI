use {
    anyhow::Result,
    bitcoin::{OutPoint, Txid},
    ord::{InscriptionId, index::event::Event},
    ordinals::{RuneId, SatPoint},
    serde::{Deserialize, Serialize},
};

/// Serializable mirror of [`ord::index::event::Event`], logged to the journal.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum RawEvent {
    InscriptionCreated {
        block_height: u32,
        charms: u16,
        inscription_id: InscriptionId,
        location: Option<SatPoint>,
        parent_inscription_ids: Vec<InscriptionId>,
        sequence_number: u32,
    },
    InscriptionTransferred {
        block_height: u32,
        inscription_id: InscriptionId,
        new_location: SatPoint,
        old_location: SatPoint,
        sequence_number: u32,
    },
    RuneBurned {
        amount: u128,
        block_height: u32,
        rune_id: RuneId,
        txid: Txid,
    },
    RuneEtched {
        block_height: u32,
        rune_id: RuneId,
        txid: Txid,
    },
    RuneMinted {
        amount: u128,
        block_height: u32,
        rune_id: RuneId,
        txid: Txid,
    },
    RuneTransferred {
        amount: u128,
        block_height: u32,
        outpoint: OutPoint,
        rune_id: RuneId,
        txid: Txid,
    },
}

impl From<Event> for RawEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::InscriptionCreated {
                block_height,
                charms,
                inscription_id,
                location,
                parent_inscription_ids,
                sequence_number,
            } => Self::InscriptionCreated {
                block_height,
                charms,
                inscription_id,
                location,
                parent_inscription_ids,
                sequence_number,
            },
            Event::InscriptionTransferred {
                block_height,
                inscription_id,
                new_location,
                old_location,
                sequence_number,
            } => Self::InscriptionTransferred {
                block_height,
                inscription_id,
                new_location,
                old_location,
                sequence_number,
            },
            Event::RuneBurned {
                amount,
                block_height,
                rune_id,
                txid,
            } => Self::RuneBurned {
                amount,
                block_height,
                rune_id,
                txid,
            },
            Event::RuneEtched {
                block_height,
                rune_id,
                txid,
            } => Self::RuneEtched {
                block_height,
                rune_id,
                txid,
            },
            Event::RuneMinted {
                amount,
                block_height,
                rune_id,
                txid,
            } => Self::RuneMinted {
                amount,
                block_height,
                rune_id,
                txid,
            },
            Event::RuneTransferred {
                amount,
                block_height,
                outpoint,
                rune_id,
                txid,
            } => Self::RuneTransferred {
                amount,
                block_height,
                outpoint,
                rune_id,
                txid,
            },
        }
    }
}

impl RawEvent {
    /// Block height the event was emitted at.
    pub fn height(&self) -> u32 {
        match self {
            Self::InscriptionCreated { block_height, .. }
            | Self::InscriptionTransferred { block_height, .. }
            | Self::RuneBurned { block_height, .. }
            | Self::RuneEtched { block_height, .. }
            | Self::RuneMinted { block_height, .. }
            | Self::RuneTransferred { block_height, .. } => *block_height,
        }
    }

    /// Encodes the event with postcard. Serializing these fields does not fail.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("RawEvent serializes to postcard")
    }

    /// Decodes an event produced by [`RawEvent::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txid() -> Txid {
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
            .parse()
            .unwrap()
    }

    fn inscription_id(index: u8) -> InscriptionId {
        format!("{}i{index}", txid()).parse().unwrap()
    }

    fn satpoint(offset: u64) -> SatPoint {
        SatPoint {
            outpoint: OutPoint {
                txid: txid(),
                vout: 1,
            },
            offset,
        }
    }

    fn rune_id() -> RuneId {
        RuneId::new(840_000, 3).unwrap()
    }

    fn samples() -> Vec<RawEvent> {
        vec![
            RawEvent::InscriptionCreated {
                block_height: 800_000,
                charms: 0b101,
                inscription_id: inscription_id(0),
                location: Some(satpoint(42)),
                parent_inscription_ids: vec![inscription_id(1), inscription_id(2)],
                sequence_number: 7,
            },
            RawEvent::InscriptionCreated {
                block_height: 800_001,
                charms: 0,
                inscription_id: inscription_id(3),
                location: None,
                parent_inscription_ids: Vec::new(),
                sequence_number: 8,
            },
            RawEvent::InscriptionTransferred {
                block_height: 800_002,
                inscription_id: inscription_id(0),
                new_location: satpoint(0),
                old_location: satpoint(42),
                sequence_number: 7,
            },
            RawEvent::RuneBurned {
                amount: u128::MAX,
                block_height: 800_003,
                rune_id: rune_id(),
                txid: txid(),
            },
            RawEvent::RuneEtched {
                block_height: 800_004,
                rune_id: rune_id(),
                txid: txid(),
            },
            RawEvent::RuneMinted {
                amount: 1_000,
                block_height: 800_005,
                rune_id: rune_id(),
                txid: txid(),
            },
            RawEvent::RuneTransferred {
                amount: 250,
                block_height: 800_006,
                outpoint: OutPoint {
                    txid: txid(),
                    vout: 2,
                },
                rune_id: rune_id(),
                txid: txid(),
            },
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for event in samples() {
            let bytes = event.encode();
            assert_eq!(RawEvent::decode(&bytes).unwrap(), event);
        }
    }
}
