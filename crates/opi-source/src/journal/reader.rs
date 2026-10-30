use {
    super::{
        INSCRIPTION_ID_LEN, Journal, cf, decode_info, decode_inscription_move, decode_rune_etching,
        decode_rune_event, decode_rune_spend, decode_transfer, decompress_info, segment_tip_in,
        table,
    },
    anyhow::{Result, bail},
    bitcoin::{block::Header, consensus},
    opi_core::{
        BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionMove, InscriptionRecord,
        RuneFeed,
    },
    redb::{ReadTransaction, ReadableDatabase},
    std::collections::BTreeMap,
};

/// [`BlockSource`] over the finalized part of a journal. Clones share the
/// journal.
#[derive(Clone)]
pub struct JournalReader {
    journal: Journal,
}

impl JournalReader {
    pub fn new(journal: Journal) -> JournalReader {
        JournalReader { journal }
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// A read transaction on the segment holding `height`, `None` when the
    /// segment has no finalized height at or above `height`. Fails for a
    /// height below the pruned floor.
    fn snapshot(&self, height: u32) -> Result<Option<ReadTransaction>> {
        let floor = self.journal.pruned_below()?;
        if height < floor {
            bail!(
                "height {height} is below the journal's pruned floor {floor}; a module that \
                 starts or resumes below it needs a journal and ord data dir indexed again \
                 from the start without OPI_PRUNE"
            );
        }
        let Some(segment) = self.journal.segment(height)? else {
            return Ok(None);
        };
        let tx = segment.begin_read()?;
        if segment_tip_in(&tx)?.is_none_or(|tip| height > tip) {
            return Ok(None);
        }
        Ok(Some(tx))
    }
}

/// The rows of `height` in table `name`, in key order.
fn height_rows<T>(
    tx: &ReadTransaction,
    name: &'static str,
    height: u32,
    mut decode: impl FnMut(&[u8], &[u8]) -> Result<T>,
) -> Result<Vec<T>> {
    let first = height.to_be_bytes();
    let next = height.checked_add(1).map(u32::to_be_bytes);
    let table = tx.open_table(table(name))?;
    let rows = match &next {
        Some(next) => table.range::<&[u8]>(first.as_slice()..next.as_slice())?,
        None => table.range::<&[u8]>(first.as_slice()..)?,
    };
    rows.map(|row| {
        let (key, value) = row?;
        decode(key.value(), value.value())
    })
    .collect()
}

fn block_info_in(tx: &ReadTransaction, height: u32) -> Result<Option<BlockInfo>> {
    let headers = tx.open_table(table(cf::HEADERS))?;
    let Some(header) = headers.get(height.to_be_bytes().as_slice())? else {
        return Ok(None);
    };
    let header: Header = consensus::deserialize(header.value())?;
    Ok(Some(BlockInfo {
        height,
        hash: header.block_hash().to_string(),
        time: header.time,
    }))
}

fn inscription_records_in(tx: &ReadTransaction, height: u32) -> Result<Vec<InscriptionRecord>> {
    height_rows(tx, cf::TRANSFERS, height, decode_transfer)
}

/// Rune events but the inputs, etchings and spends of `height`, each in key
/// order.
fn rune_feed_in(tx: &ReadTransaction, height: u32) -> Result<RuneFeed> {
    Ok(RuneFeed {
        events: height_rows(tx, cf::RUNE_EVENTS, height, |_, value| {
            decode_rune_event(value)
        })?,
        etchings: height_rows(tx, cf::RUNE_ETCHINGS, height, |_, value| {
            decode_rune_etching(value)
        })?,
        spent: height_rows(tx, cf::RUNE_SPENT, height, |_, value| {
            decode_rune_spend(value)
        })?,
    })
}

/// Inscription moves of `height` in emission order.
fn inscription_moves_in(tx: &ReadTransaction, height: u32) -> Result<Vec<InscriptionMove>> {
    height_rows(tx, cf::INSCRIPTION_MOVES, height, |_, value| {
        decode_inscription_move(value)
    })
}

impl BlockSource for JournalReader {
    fn tip(&self) -> Result<Option<u32>> {
        self.journal.final_tip()
    }

    fn block_info(&self, height: u32) -> Result<Option<BlockInfo>> {
        if height < self.journal.pruned_below()? {
            return Ok(None);
        }
        match self.journal.segment(height)? {
            Some(segment) => block_info_in(&segment.begin_read()?, height),
            None => Ok(None),
        }
    }

    fn inscription_records(&self, height: u32) -> Result<Vec<InscriptionRecord>> {
        match self.snapshot(height)? {
            Some(tx) => inscription_records_in(&tx, height),
            None => Ok(Vec::new()),
        }
    }

    fn block_view(&self, height: u32) -> Result<Option<BlockView>> {
        let Some(tx) = self.snapshot(height)? else {
            return Ok(None);
        };
        let Some(block) = block_info_in(&tx, height)? else {
            return Ok(None);
        };
        let records = inscription_records_in(&tx, height)?;
        let infos = height_rows(&tx, cf::INFO, height, |key, value| {
            let id = decode_inscription_id_key(&key[4..])?;
            Ok((id.clone(), decode_info(&id, &decompress_info(value)?)?))
        })?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        // Every creation carries its info; a first transfer only when the
        // brc20 module acts on it.
        if let Some(record) = records.iter().find(|record| {
            record.old_satpoint.is_none() && !infos.contains_key(&record.inscription_id)
        }) {
            bail!(
                "inscription {} created at height {height} has no info row",
                record.inscription_id
            );
        }
        Ok(Some(BlockView {
            height,
            hash: block.hash,
            time: block.time,
            inscriptions: InscriptionFeed { records, infos },
            runes: if self.journal.runes() {
                rune_feed_in(&tx, height)?
            } else {
                RuneFeed::default()
            },
            inscription_moves: if self.journal.inscriptions() {
                inscription_moves_in(&tx, height)?
            } else {
                Vec::new()
            },
        }))
    }
}

/// The `txidi<index>` form of a stored inscription id.
fn decode_inscription_id_key(raw: &[u8]) -> Result<String> {
    anyhow::ensure!(
        raw.len() == INSCRIPTION_ID_LEN,
        "inscription id is {} bytes",
        raw.len()
    );
    let mut txid = raw[..32].to_vec();
    txid.reverse();
    let index = u32::from_be_bytes(raw[32..].try_into()?);
    Ok(format!("{}i{index}", hex::encode(txid)))
}
