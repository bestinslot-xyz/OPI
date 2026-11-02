use {
    super::{
        HeightFile, INSCRIPTION_ID_LEN, Journal, Stream, cf, decode_info, decode_inscription_move,
        decode_rune_etching, decode_rune_event, decode_rune_spend, decode_transfer,
        decompress_info,
    },
    anyhow::{Result, bail},
    bitcoin::{block::Header, consensus},
    opi_core::{
        BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionMove, InscriptionRecord,
        RuneFeed,
    },
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

    /// The record file of `height`, `None` above the final tip. Fails for a
    /// height below the pruned floor.
    fn records(&self, height: u32) -> Result<Option<HeightFile>> {
        self.check_floor(height)?;
        if self.journal.final_tip()?.is_none_or(|tip| height > tip) {
            return Ok(None);
        }
        let file = self.journal.records(height)?;
        if file.is_none() {
            // A prune may have raised the floor past `height` since the
            // check.
            self.check_floor(height)?;
        }
        Ok(file)
    }

    fn check_floor(&self, height: u32) -> Result<()> {
        let floor = self.journal.pruned_below()?;
        if height < floor {
            bail!(
                "height {height} is below the journal's pruned floor {floor}; a module that \
                 starts or resumes below it needs a journal and ord data dir indexed again \
                 from the start without OPI_PRUNE"
            );
        }
        Ok(())
    }
}

fn block_info_of(height: u32, head: &[u8]) -> Result<BlockInfo> {
    let header: Header = consensus::deserialize(head)?;
    Ok(BlockInfo {
        height,
        hash: header.block_hash().to_string(),
        time: header.time,
    })
}

fn inscription_records_in(file: &HeightFile) -> Result<Vec<InscriptionRecord>> {
    file.rows(cf::TRANSFERS)
        .map(|(key, value)| decode_transfer(key, value))
        .collect()
}

fn values<T>(file: &HeightFile, name: &str, decode: impl Fn(&[u8]) -> Result<T>) -> Result<Vec<T>> {
    file.rows(name).map(|(_, value)| decode(value)).collect()
}

/// Rune events but the inputs, etchings and spends of a height, each in key
/// order.
fn rune_feed_in(file: &HeightFile) -> Result<RuneFeed> {
    Ok(RuneFeed {
        events: values(file, cf::RUNE_EVENTS, decode_rune_event)?,
        etchings: values(file, cf::RUNE_ETCHINGS, decode_rune_etching)?,
        spent: values(file, cf::RUNE_SPENT, decode_rune_spend)?,
    })
}

/// Inscription moves of a height in emission order.
fn inscription_moves_in(file: &HeightFile) -> Result<Vec<InscriptionMove>> {
    values(file, cf::INSCRIPTION_MOVES, decode_inscription_move)
}

impl BlockSource for JournalReader {
    fn tip(&self) -> Result<Option<u32>> {
        self.journal.final_tip()
    }

    fn block_info(&self, height: u32) -> Result<Option<BlockInfo>> {
        if height < self.journal.pruned_below()? {
            return Ok(None);
        }
        match self.journal.store().read_head(Stream::Records, height)? {
            Some(head) => Ok(Some(block_info_of(height, &head)?)),
            None => Ok(None),
        }
    }

    fn inscription_records(&self, height: u32) -> Result<Vec<InscriptionRecord>> {
        match self.records(height)? {
            Some(file) => inscription_records_in(&file),
            None => Ok(Vec::new()),
        }
    }

    fn block_view(&self, height: u32) -> Result<Option<BlockView>> {
        let Some(file) = self.records(height)? else {
            return Ok(None);
        };
        let block = block_info_of(height, &file.head)?;
        let records = inscription_records_in(&file)?;
        let infos = file
            .rows(cf::INFO)
            .map(|(key, value)| {
                let id = decode_inscription_id_key(&key[4..])?;
                Ok((id.clone(), decode_info(&id, &decompress_info(value)?)?))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
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
                rune_feed_in(&file)?
            } else {
                RuneFeed::default()
            },
            inscription_moves: if self.journal.inscriptions() {
                inscription_moves_in(&file)?
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
