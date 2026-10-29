//! Synthetic journal contents for tests and measurements, behind the
//! `testing` feature.

use {
    super::{
        Journal, cf, compress_info, encode_info, encode_transfer, info_key, raw_event_key,
        set_final_tip, table, transfer_key,
    },
    anyhow::Result,
    opi_core::{InscriptionInfo, InscriptionRecord},
    std::ops::Range,
};

/// The shape of each height [`fill`] writes.
#[derive(Debug, Clone, Copy)]
pub struct Density {
    /// Raw events per height.
    pub raw_events: u32,
    /// Inscriptions created per height, each with an info row.
    pub creations: u32,
    /// First transfers per height, each of an inscription created at the
    /// height before, which consumes that inscription's info row.
    pub transfers: u32,
    /// Bytes of content per info row, incompressible.
    pub content: usize,
}

/// The id of the `index`th inscription created at `height`.
pub fn inscription_id(height: u32, index: u32) -> String {
    format!("{height:056x}{index:08x}i0")
}

/// Writes `heights` with `density`, then sets the final tip to the last of
/// them. Each height commits on its own.
pub fn fill(journal: &Journal, heights: Range<u32>, density: Density) -> Result<()> {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for height in heights.clone() {
        let tx = journal.begin_write()?;
        {
            let mut events = tx.open_table(table(cf::RAW_EVENTS))?;
            for seq in 0..density.raw_events {
                let mut event = [0u8; 120];
                event[..4].copy_from_slice(&height.to_be_bytes());
                event[4..8].copy_from_slice(&seq.to_be_bytes());
                events.insert(raw_event_key(height, seq).as_slice(), event.as_slice())?;
            }
            tx.open_table(table(cf::RAW_HEIGHTS))?
                .insert(height.to_be_bytes().as_slice(), [0u8; 32].as_slice())?;

            let mut transfers = tx.open_table(table(cf::TRANSFERS))?;
            let mut infos = tx.open_table(table(cf::INFO))?;
            let mut idx = 0;
            for index in 0..density.creations {
                let id = inscription_id(height, index);
                let txid = id[..64].to_string();
                transfers.insert(
                    transfer_key(height, idx).as_slice(),
                    encode_transfer(&record(height, idx, &id, None, &txid))?.as_slice(),
                )?;
                idx += 1;
                let content = (0..density.content)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed as u8
                    })
                    .collect();
                let info = InscriptionInfo {
                    inscription_id: id.clone(),
                    inscription_number: 0,
                    cursed_for_brc20: false,
                    parent_id: None,
                    is_json: false,
                    content,
                    content_type: b"text/plain".to_vec(),
                    metaprotocol: Vec::new(),
                };
                infos.insert(
                    info_key(&id)?.as_slice(),
                    compress_info(&encode_info(&info)?)?.as_slice(),
                )?;
            }
            if let Some(previous) = height.checked_sub(1).filter(|h| heights.contains(h)) {
                for index in 0..density.transfers.min(density.creations) {
                    let id = inscription_id(previous, index);
                    let txid = inscription_id(height, density.creations + index)[..64].to_string();
                    let old = format!("{}:0:0", &id[..64]);
                    transfers.insert(
                        transfer_key(height, idx).as_slice(),
                        encode_transfer(&record(height, idx, &id, Some(old), &txid))?.as_slice(),
                    )?;
                    idx += 1;
                }
            }
        }
        set_final_tip(&tx, Some(height))?;
        tx.commit()?;
    }
    Ok(())
}

fn record(
    height: u32,
    idx: u32,
    id: &str,
    old_satpoint: Option<String>,
    txid: &str,
) -> InscriptionRecord {
    InscriptionRecord {
        height,
        idx,
        inscription_id: id.to_string(),
        old_satpoint,
        new_satpoint: format!("{txid}:0:0"),
        sent_as_fee: false,
        new_output_value: 546,
        txid: txid.to_string(),
        new_pkscript: vec![0x51; 34],
    }
}
