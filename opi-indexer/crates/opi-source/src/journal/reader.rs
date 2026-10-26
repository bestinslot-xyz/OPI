use {
    super::{
        Journal, RUNE_ID_LEN, cf, decode_info, decode_rune_counter_log, decode_rune_etching,
        decode_rune_event, decode_transfer, decompress_info, final_tip_in, info_key,
        rune_etching_key, rune_event_key, rune_id_string, table, transfer_key,
    },
    anyhow::{Result, anyhow},
    bitcoin::{block::Header, consensus},
    opi_core::{
        BlockInfo, BlockSource, BlockView, InscriptionFeed, InscriptionInfo, InscriptionRecord,
        RuneCounter, RuneFeed,
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

/// Rune events, etchings and counter rows (after-totals) of `height`, each
/// in key order.
fn rune_feed_in(tx: &ReadTransaction, height: u32) -> Result<RuneFeed> {
    let events = tx
        .open_table(table(cf::RUNE_EVENTS))?
        .range::<&[u8]>(
            rune_event_key(height, 0).as_slice()..=rune_event_key(height, u32::MAX).as_slice(),
        )?
        .map(|row| decode_rune_event(row?.1.value()))
        .collect::<Result<_>>()?;
    let etchings = tx
        .open_table(table(cf::RUNE_ETCHINGS))?
        .range::<&[u8]>(
            rune_etching_key(height, 0).as_slice()..=rune_etching_key(height, u32::MAX).as_slice(),
        )?
        .map(|row| decode_rune_etching(row?.1.value()))
        .collect::<Result<_>>()?;

    let mut first = [0; 4 + RUNE_ID_LEN];
    first[..4].copy_from_slice(&height.to_be_bytes());
    let mut last = [0xff; 4 + RUNE_ID_LEN];
    last[..4].copy_from_slice(&height.to_be_bytes());
    let counters = tx
        .open_table(table(cf::RUNE_COUNTER_LOG))?
        .range::<&[u8]>(first.as_slice()..=last.as_slice())?
        .map(|row| {
            let (key, value) = row?;
            let (_, (burned, mints)) = decode_rune_counter_log(value.value())?;
            Ok(RuneCounter {
                rune_id: rune_id_string(&key.value()[4..]),
                burned,
                mints,
            })
        })
        .collect::<Result<_>>()?;
    Ok(RuneFeed {
        events,
        etchings,
        counters,
    })
}

fn inscription_info_in(
    tx: &ReadTransaction,
    inscription_id: &str,
) -> Result<Option<InscriptionInfo>> {
    let infos = tx.open_table(table(cf::INFO))?;
    let Some(stored) = infos.get(info_key(inscription_id)?.as_slice())? else {
        return Ok(None);
    };
    Ok(Some(decode_info(
        inscription_id,
        &decompress_info(stored.value())?,
    )?))
}

impl BlockSource for JournalReader {
    fn tip(&self) -> Result<Option<u32>> {
        self.journal.final_tip()
    }

    fn block_info(&self, height: u32) -> Result<Option<BlockInfo>> {
        block_info_in(&self.journal.db().begin_read()?, height)
    }

    fn inscription_records(&self, height: u32) -> Result<Vec<InscriptionRecord>> {
        inscription_records_in(&self.journal.db().begin_read()?, height)
    }

    fn inscription_info(&self, inscription_id: &str) -> Result<Option<InscriptionInfo>> {
        inscription_info_in(&self.journal.db().begin_read()?, inscription_id)
    }

    fn block_view(&self, height: u32) -> Result<Option<BlockView>> {
        let tx = self.journal.db().begin_read()?;
        if final_tip_in(&tx)?.is_none_or(|tip| height > tip) {
            return Ok(None);
        }
        let Some(block) = block_info_in(&tx, height)? else {
            return Ok(None);
        };
        let records = inscription_records_in(&tx, height)?;
        let mut infos = BTreeMap::new();
        for record in &records {
            if infos.contains_key(&record.inscription_id) {
                continue;
            }
            let info = inscription_info_in(&tx, &record.inscription_id)?.ok_or_else(|| {
                anyhow!(
                    "inscription {} of height {height} has no info row",
                    record.inscription_id
                )
            })?;
            infos.insert(record.inscription_id.clone(), info);
        }
        Ok(Some(BlockView {
            height,
            hash: block.hash,
            time: block.time,
            inscriptions: InscriptionFeed { records, infos },
            runes: rune_feed_in(&tx, height)?,
        }))
    }
}
