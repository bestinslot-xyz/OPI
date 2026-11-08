//! Synthetic journal contents for tests and measurements, behind the
//! `testing` feature.

use {
    super::{
        HeightFile, INSCRIPTION_ID_LEN, Journal, Rows, Stream, cf, compress_info, encode_info,
        encode_transfer, info_key, transfer_key,
    },
    anyhow::Result,
    bitcoin::{BlockHash, hashes::Hash},
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
    /// height before, with its info row.
    pub transfers: u32,
    /// Bytes of content per info row, incompressible.
    pub content: usize,
}

/// The id of the `index`th inscription created at `height`.
pub fn inscription_id(height: u32, index: u32) -> String {
    format!("{height:056x}{index:08x}i0")
}

/// Writes `heights` with `density`, each height's raw events and then its
/// records, and makes the last of them the final tip.
pub fn fill(journal: &Journal, heights: Range<u32>, density: Density) -> Result<()> {
    for height in heights {
        journal.write_raw(height, BlockHash::all_zeros(), &raw_events(height, density))?;
        journal.append_records(&records_file(height, density)?)?;
    }
    Ok(())
}

/// The raw events [`fill`] writes for `height`.
pub fn raw_events(height: u32, density: Density) -> Vec<Vec<u8>> {
    (0..density.raw_events)
        .map(|seq| {
            let mut event = vec![0u8; 120];
            event[..4].copy_from_slice(&height.to_be_bytes());
            event[4..8].copy_from_slice(&seq.to_be_bytes());
            event
        })
        .collect()
}

/// The record file [`fill`] writes for `height`. Its contents depend only on
/// `height` and `density`.
pub fn records_file(height: u32, density: Density) -> Result<HeightFile> {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(height).wrapping_mul(0x2545_f491);
    let mut content = || {
        (0..density.content)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect::<Vec<_>>()
    };
    let mut transfers = Rows::default();
    let mut infos = Rows::default();
    let mut put_info = |id: &str, content: Vec<u8>| -> Result<()> {
        let mut key = [0; 4 + INSCRIPTION_ID_LEN];
        key[..4].copy_from_slice(&height.to_be_bytes());
        key[4..].copy_from_slice(&info_key(id)?);
        let info = InscriptionInfo {
            inscription_id: id.to_string(),
            inscription_number: 0,
            cursed_for_brc20: false,
            parent_id: None,
            is_json: false,
            content,
            content_type: b"text/plain".to_vec(),
            metaprotocol: Vec::new(),
        };
        infos.insert(&key, &compress_info(&encode_info(&info)?)?)
    };
    let mut idx = 0;
    for index in 0..density.creations {
        let id = inscription_id(height, index);
        let txid = id[..64].to_string();
        transfers.insert(
            &transfer_key(height, idx),
            &encode_transfer(&record(height, idx, &id, None, &txid))?,
        )?;
        idx += 1;
        put_info(&id, content())?;
    }
    if let Some(previous) = height.checked_sub(1) {
        for index in 0..density.transfers.min(density.creations) {
            let id = inscription_id(previous, index);
            let txid = inscription_id(height, density.creations + index)[..64].to_string();
            let old = format!("{}:0:0", &id[..64]);
            transfers.insert(
                &transfer_key(height, idx),
                &encode_transfer(&record(height, idx, &id, Some(old), &txid))?,
            )?;
            idx += 1;
            put_info(&id, content())?;
        }
    }
    let mut file = HeightFile::new(height, header(height));
    *file.rows_mut(cf::TRANSFERS) = transfers;
    *file.rows_mut(cf::INFO) = infos;
    Ok(file)
}

/// Writes `file` over the record file of its height.
pub fn rewrite_records(journal: &Journal, file: &HeightFile) -> Result<()> {
    journal.store().write(Stream::Records, file)
}

/// Where a kill stops a height file's write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillPoint {
    /// Part way through writing the `.tmp` file.
    MidWrite,
    /// After the `.tmp` file is written and synced, before its rename.
    BeforeRename,
    /// After the rename, before the directory sync: the file is in place.
    AfterRename,
}

/// Leaves `file` in `stream` as a kill at `point` of its write would.
pub fn killed_write(
    journal: &Journal,
    stream: Stream,
    file: &HeightFile,
    point: KillPoint,
) -> Result<()> {
    let bytes = file.encode(stream)?;
    let path = journal.height_path(stream, file.height);
    std::fs::create_dir_all(path.parent().expect("a height file has a segment dir"))?;
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    match point {
        KillPoint::MidWrite => std::fs::write(tmp, &bytes[..bytes.len() / 2])?,
        KillPoint::BeforeRename => std::fs::write(tmp, &bytes)?,
        KillPoint::AfterRename => std::fs::write(path, &bytes)?,
    }
    Ok(())
}

/// The bytes `file` takes on disk in `stream`.
pub fn encoded_len(file: &HeightFile, stream: Stream) -> Result<u64> {
    Ok(file.encode(stream)?.len() as u64)
}

/// A block header unique to `height`, whose hash ord's index will not hold.
pub fn header(height: u32) -> Vec<u8> {
    let header = bitcoin::block::Header {
        version: bitcoin::block::Version::ONE,
        prev_blockhash: BlockHash::all_zeros(),
        merkle_root: bitcoin::TxMerkleNode::all_zeros(),
        time: height,
        bits: bitcoin::CompactTarget::from_consensus(0),
        nonce: height,
    };
    bitcoin::consensus::serialize(&header)
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
