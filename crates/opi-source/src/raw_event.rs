use {
    anyhow::Result,
    bitcoin::{OutPoint, Txid},
    ord::{InscriptionId, index::event::Event},
    ordinals::{RuneId, SatPoint},
    serde::{Deserialize, Deserializer, Serialize, Serializer},
};

/// Serializable mirror of [`ord::index::event::Event`], logged to the journal.
///
/// Ord identifiers are stored as their fields in binary form, not as their
/// display strings. `Txid` and `OutPoint` use bitcoin's own serde form.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum RawEvent {
    InscriptionCreated {
        block_height: u32,
        charms: u16,
        #[serde(with = "inscription_id")]
        inscription_id: InscriptionId,
        #[serde(with = "option_satpoint")]
        location: Option<SatPoint>,
        #[serde(with = "inscription_id_seq")]
        parent_inscription_ids: Vec<InscriptionId>,
        sequence_number: u32,
    },
    InscriptionTransferred {
        block_height: u32,
        #[serde(with = "inscription_id")]
        inscription_id: InscriptionId,
        #[serde(with = "satpoint")]
        new_location: SatPoint,
        #[serde(with = "satpoint")]
        old_location: SatPoint,
        sequence_number: u32,
    },
    RuneBurned {
        amount: u128,
        block_height: u32,
        #[serde(with = "rune_id")]
        rune_id: RuneId,
        txid: Txid,
    },
    RuneEtched {
        block_height: u32,
        #[serde(with = "rune_id")]
        rune_id: RuneId,
        txid: Txid,
    },
    RuneMinted {
        amount: u128,
        block_height: u32,
        #[serde(with = "rune_id")]
        rune_id: RuneId,
        txid: Txid,
    },
    RuneTransferred {
        amount: u128,
        block_height: u32,
        outpoint: OutPoint,
        #[serde(with = "rune_id")]
        rune_id: RuneId,
        txid: Txid,
    },
}

/// Binary layout of an [`InscriptionId`]: its txid and index.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct InscriptionIdFields {
    txid: Txid,
    index: u32,
}

impl From<InscriptionId> for InscriptionIdFields {
    fn from(id: InscriptionId) -> Self {
        Self {
            txid: id.txid,
            index: id.index,
        }
    }
}

impl From<InscriptionIdFields> for InscriptionId {
    fn from(fields: InscriptionIdFields) -> Self {
        Self {
            txid: fields.txid,
            index: fields.index,
        }
    }
}

/// Binary layout of a [`SatPoint`]: its outpoint and offset.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct SatPointFields {
    outpoint: OutPoint,
    offset: u64,
}

impl From<SatPoint> for SatPointFields {
    fn from(point: SatPoint) -> Self {
        Self {
            outpoint: point.outpoint,
            offset: point.offset,
        }
    }
}

impl From<SatPointFields> for SatPoint {
    fn from(fields: SatPointFields) -> Self {
        Self {
            outpoint: fields.outpoint,
            offset: fields.offset,
        }
    }
}

/// Binary layout of a [`RuneId`]: its block and transaction index.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct RuneIdFields {
    block: u64,
    tx: u32,
}

impl From<RuneId> for RuneIdFields {
    fn from(id: RuneId) -> Self {
        Self {
            block: id.block,
            tx: id.tx,
        }
    }
}

impl From<RuneIdFields> for RuneId {
    fn from(fields: RuneIdFields) -> Self {
        Self {
            block: fields.block,
            tx: fields.tx,
        }
    }
}

mod inscription_id {
    use super::*;

    pub fn serialize<S: Serializer>(id: &InscriptionId, s: S) -> Result<S::Ok, S::Error> {
        InscriptionIdFields::from(*id).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<InscriptionId, D::Error> {
        InscriptionIdFields::deserialize(d).map(InscriptionId::from)
    }
}

mod inscription_id_seq {
    use super::*;

    pub fn serialize<S: Serializer>(ids: &[InscriptionId], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(ids.iter().map(|id| InscriptionIdFields::from(*id)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<InscriptionId>, D::Error> {
        Vec::<InscriptionIdFields>::deserialize(d)
            .map(|fields| fields.into_iter().map(InscriptionId::from).collect())
    }
}

mod satpoint {
    use super::*;

    pub fn serialize<S: Serializer>(point: &SatPoint, s: S) -> Result<S::Ok, S::Error> {
        SatPointFields::from(*point).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SatPoint, D::Error> {
        SatPointFields::deserialize(d).map(SatPoint::from)
    }
}

mod option_satpoint {
    use super::*;

    pub fn serialize<S: Serializer>(point: &Option<SatPoint>, s: S) -> Result<S::Ok, S::Error> {
        point
            .as_ref()
            .map(|point| SatPointFields::from(*point))
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SatPoint>, D::Error> {
        Option::<SatPointFields>::deserialize(d).map(|fields| fields.map(SatPoint::from))
    }
}

mod rune_id {
    use super::*;

    pub fn serialize<S: Serializer>(id: &RuneId, s: S) -> Result<S::Ok, S::Error> {
        RuneIdFields::from(*id).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RuneId, D::Error> {
        RuneIdFields::deserialize(d).map(RuneId::from)
    }
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

    /// The encodings of [`samples`], one per variant plus a creation without a
    /// location. These bytes are the on-disk format of the permanent raw
    /// event log and must not change: a change makes stored events
    /// undecodable or decodes them wrongly.
    const PINNED: [&str; 7] = [
        "0080ea3005206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d61900000000000001206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000012a02206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d619000000000001206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d61900000000000207",
        "0081ea3000206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d619000000000003000008",
        "0182ea30206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d619000000000000206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d61900000000000100206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000012a07",
        "02ffffffffffffffffffffffffffffffffffff0383ea30c0a23303206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000",
        "0384ea30c0a23303206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000",
        "04e80785ea30c0a23303206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000",
        "05fa0186ea30206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d619000000000002c0a23303206fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000",
    ];

    #[test]
    fn raw_event_encoding_is_pinned() {
        let encoded: Vec<String> = samples()
            .iter()
            .map(|event| hex::encode(event.encode()))
            .collect();
        assert_eq!(encoded, PINNED, "{encoded:#?}");
        for (pinned, event) in PINNED.iter().zip(samples()) {
            assert_eq!(
                RawEvent::decode(&hex::decode(pinned).unwrap()).unwrap(),
                event
            );
        }
    }

    #[test]
    fn encoding_is_compact() {
        let event = RawEvent::InscriptionTransferred {
            block_height: 800_002,
            inscription_id: inscription_id(0),
            new_location: satpoint(0),
            old_location: satpoint(42),
            sequence_number: 7,
        };

        let len = event.encode().len();
        assert!(len < 140, "InscriptionTransferred encodes to {len} bytes");
    }

    #[test]
    fn txid_and_outpoint_use_binary_form() {
        let txid_len = postcard::to_allocvec(&txid()).unwrap().len();
        let outpoint = OutPoint {
            txid: txid(),
            vout: 1,
        };
        let outpoint_len = postcard::to_allocvec(&outpoint).unwrap().len();

        assert_eq!((txid_len, outpoint_len), (33, 34));
    }
}
