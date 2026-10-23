use {
    super::{
        Journal, cf, decode_info, decode_transfer, decompress_info, info_key, table, transfer_key,
    },
    anyhow::Result,
    bitcoin::{block::Header, consensus},
    opi_core::{BlockInfo, BlockSource, InscriptionInfo, InscriptionRecord},
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
}

impl BlockSource for JournalReader {
    fn tip(&self) -> Result<Option<u32>> {
        self.journal.final_tip()
    }

    fn block_info(&self, height: u32) -> Result<Option<BlockInfo>> {
        let tx = self.journal.db().begin_read()?;
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

    fn inscription_records(&self, height: u32) -> Result<Vec<InscriptionRecord>> {
        let tx = self.journal.db().begin_read()?;
        let transfers = tx.open_table(table(cf::TRANSFERS))?;
        let first = transfer_key(height, 0);
        let last = transfer_key(height, u32::MAX);
        transfers
            .range::<&[u8]>(first.as_slice()..=last.as_slice())?
            .map(|row| {
                let (key, value) = row?;
                decode_transfer(key.value(), value.value())
            })
            .collect()
    }

    fn inscription_info(&self, inscription_id: &str) -> Result<Option<InscriptionInfo>> {
        let tx = self.journal.db().begin_read()?;
        let infos = tx.open_table(table(cf::INFO))?;
        let Some(stored) = infos.get(info_key(inscription_id)?.as_slice())? else {
            return Ok(None);
        };
        Ok(Some(decode_info(
            inscription_id,
            &decompress_info(stored.value())?,
        )?))
    }
}
