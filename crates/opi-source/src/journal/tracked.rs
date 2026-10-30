//! `tracked.redb`: the bound text and JSON inscriptions whose first transfer
//! the journal still has to record.
//!
//! The fork records an inscription's creation and its first transfer only,
//! and only for an inscription created bound with text or JSON content.
//! Whether a transfer is such a first transfer decides the idx of every
//! record numbered after it, and ord's index keeps no content type, so the
//! journal keeps one row per tracked inscription: the height it was created
//! at, the height of its first transfer once there is one, and, for an
//! inscription the brc20 module acts on when it is transferred
//! ([`opi_core::brc20::acts_on_transfer`]), the info its creation recorded,
//! which its first transfer's record carries again. No other module reads a
//! transfer's info, so the first transfer of any other inscription carries
//! none.
//!
//! A log keyed by height names every row a height created or transferred,
//! so a rollback undoes them and a prune drops the rows of inscriptions
//! transferred below the window. Writes for a height commit before the
//! segment that finalizes it, and finalize first undoes whatever is logged
//! above the final tip, so the set always matches the final tip.

use {
    super::{Journal, begin_write, create_db, info_key, table},
    anyhow::{Context, Result, ensure},
    ord::InscriptionId,
    redb::{Database, ReadableDatabase, ReadableTable},
    std::{collections::HashMap, path::Path},
};

/// `tracked.redb`'s file name inside the journal dir.
pub const TRACKED_FILE: &str = "tracked.redb";

/// Inscription id (store form) -> row.
const TRACKED: &str = "tracked";
/// Height (`u32` BE) ++ kind ++ inscription id (store form) -> nothing.
const LOG: &str = "tracked_log";
/// The tables of `tracked.redb`.
pub const TABLES: [&str; 2] = [TRACKED, LOG];
const CREATED: u8 = 0;
const TRANSFERRED: u8 = 1;
const ID_LEN: usize = super::INSCRIPTION_ID_LEN;
const NOT_TRANSFERRED: u32 = u32::MAX;

/// A tracked inscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    /// Height it was created at.
    pub created: u32,
    /// Height of its first transfer, the one the journal records.
    pub transferred: Option<u32>,
    /// Its stored info, compressed, when the brc20 module acts on its
    /// transfer.
    pub info: Option<Vec<u8>>,
}

impl Tracked {
    /// Created height (`u32` BE) ++ transfer height (`u32` BE, all ones for
    /// none) ++ the compressed info, if any.
    fn encode(&self) -> Vec<u8> {
        let info = self.info.as_deref().unwrap_or_default();
        let mut out = Vec::with_capacity(8 + info.len());
        out.extend(self.created.to_be_bytes());
        out.extend(self.transferred.unwrap_or(NOT_TRANSFERRED).to_be_bytes());
        out.extend(info);
        out
    }

    fn decode(value: &[u8]) -> Result<Tracked> {
        ensure!(value.len() >= 8, "tracked row is {} bytes", value.len());
        let transferred = u32::from_be_bytes(value[4..8].try_into()?);
        Ok(Tracked {
            created: u32::from_be_bytes(value[..4].try_into()?),
            transferred: (transferred != NOT_TRANSFERRED).then_some(transferred),
            info: (value.len() > 8).then(|| value[8..].to_vec()),
        })
    }
}

fn id_key(id: InscriptionId) -> Result<[u8; ID_LEN]> {
    info_key(&id.to_string())
}

fn log_key(height: u32, kind: u8, id: InscriptionId) -> Result<[u8; 5 + ID_LEN]> {
    let mut key = [0; 5 + ID_LEN];
    key[..4].copy_from_slice(&height.to_be_bytes());
    key[4] = kind;
    key[5..].copy_from_slice(&id_key(id)?);
    Ok(key)
}

/// Opens `tracked.redb` at `path`, creating it with its tables when absent.
pub(super) fn open(path: &Path, cache_bytes: usize) -> Result<Database> {
    let db = create_db(path, cache_bytes)?;
    let tx = begin_write(&db)?;
    tx.open_table(table(TRACKED))?;
    tx.open_table(table(LOG))?;
    tx.commit()?;
    Ok(db)
}

/// A change finalize makes to the set.
#[derive(Debug, Clone)]
pub(super) enum Change {
    Created {
        height: u32,
        id: InscriptionId,
        info: Option<Vec<u8>>,
    },
    Transferred {
        height: u32,
        id: InscriptionId,
    },
}

/// The set as one finalize batch sees it: the rows in `tracked.redb` with
/// the batch's changes laid over them.
pub(super) struct Overlay<'a> {
    journal: &'a Journal,
    rows: HashMap<InscriptionId, Option<Tracked>>,
    changes: Vec<Change>,
}

impl<'a> Overlay<'a> {
    pub(super) fn new(journal: &'a Journal) -> Overlay<'a> {
        Overlay {
            journal,
            rows: HashMap::new(),
            changes: Vec::new(),
        }
    }

    /// The row of `id`, if it is tracked.
    pub(super) fn get(&mut self, id: InscriptionId) -> Result<Option<Tracked>> {
        if let Some(row) = self.rows.get(&id) {
            return Ok(row.clone());
        }
        let row = self.journal.tracked(id)?;
        self.rows.insert(id, row.clone());
        Ok(row)
    }

    pub(super) fn apply(&mut self, change: Change) -> Result<()> {
        match &change {
            Change::Created { height, id, info } => {
                self.rows.insert(
                    *id,
                    Some(Tracked {
                        created: *height,
                        transferred: None,
                        info: info.clone(),
                    }),
                );
            }
            Change::Transferred { height, id } => {
                let mut row = self
                    .get(*id)?
                    .with_context(|| format!("first transfer of untracked inscription {id}"))?;
                row.transferred = Some(*height);
                self.rows.insert(*id, Some(row));
            }
        }
        self.changes.push(change);
        Ok(())
    }

    /// Commits the batch's changes to `tracked.redb` in one transaction.
    pub(super) fn commit(&mut self) -> Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }
        let tx = begin_write(self.journal.tracked_db())?;
        {
            let mut rows = tx.open_table(table(TRACKED))?;
            let mut log = tx.open_table(table(LOG))?;
            for change in self.changes.drain(..) {
                match change {
                    Change::Created { height, id, info } => {
                        let row = Tracked {
                            created: height,
                            transferred: None,
                            info,
                        };
                        rows.insert(id_key(id)?.as_slice(), row.encode().as_slice())?;
                        log.insert(log_key(height, CREATED, id)?.as_slice(), [].as_slice())?;
                    }
                    Change::Transferred { height, id } => {
                        let key = id_key(id)?;
                        let mut row = Tracked::decode(
                            rows.get(key.as_slice())?
                                .with_context(|| format!("no tracked row for {id}"))?
                                .value(),
                        )?;
                        row.transferred = Some(height);
                        rows.insert(key.as_slice(), row.encode().as_slice())?;
                        log.insert(log_key(height, TRANSFERRED, id)?.as_slice(), [].as_slice())?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

impl Journal {
    /// The tracked row of `id`, if it is tracked.
    pub fn tracked(&self, id: InscriptionId) -> Result<Option<Tracked>> {
        let tx = self.tracked_db().begin_read()?;
        let rows = tx.open_table(table(TRACKED))?;
        rows.get(id_key(id)?.as_slice())?
            .map(|row| Tracked::decode(row.value()))
            .transpose()
    }

    /// Undoes every change logged above `height`: drops the rows created
    /// above it and clears the first transfers recorded above it.
    pub(super) fn tracked_undo_above(&self, height: Option<u32>) -> Result<()> {
        let from = match height {
            Some(u32::MAX) => return Ok(()),
            Some(height) => height + 1,
            None => 0,
        };
        let first = from.to_be_bytes();
        let tx = begin_write(self.tracked_db())?;
        let mut changed = false;
        {
            let mut rows = tx.open_table(table(TRACKED))?;
            let mut log = tx.open_table(table(LOG))?;
            let logged = log
                .range::<&[u8]>(first.as_slice()..)?
                .map(|row| Ok(row?.0.value().to_vec()))
                .collect::<Result<Vec<_>>>()?;
            for key in logged.iter().rev() {
                changed = true;
                let id = &key[5..];
                if key[4] == CREATED {
                    rows.remove(id)?;
                    continue;
                }
                let row = rows.get(id)?.map(|row| row.value().to_vec());
                if let Some(row) = row {
                    let mut row = Tracked::decode(&row)?;
                    row.transferred = None;
                    rows.insert(id, row.encode().as_slice())?;
                }
            }
            log.retain_in::<&[u8], _>(first.as_slice().., |_, _| false)?;
        }
        if changed {
            tx.commit()?;
        } else {
            tx.abort()?;
        }
        Ok(())
    }

    /// Drops, lowest first and at most `max_rows` log rows, what nothing
    /// will undo below `height`: the rows of inscriptions transferred below
    /// it, and the log rows of the heights below it. Returns the rows of
    /// inscriptions dropped and whether nothing is left below `height`.
    pub fn prune_tracked(&self, height: u32, max_rows: u64) -> Result<(u64, bool)> {
        let end = height.to_be_bytes();
        let tx = begin_write(self.tracked_db())?;
        let mut dropped = 0;
        let done;
        {
            let mut rows = tx.open_table(table(TRACKED))?;
            let mut log = tx.open_table(table(LOG))?;
            let budget = usize::try_from(max_rows).unwrap_or(usize::MAX);
            let logged = log
                .range::<&[u8]>(..end.as_slice())?
                .take(budget + 1)
                .map(|row| Ok(row?.0.value().to_vec()))
                .collect::<Result<Vec<_>>>()?;
            done = logged.len() <= budget;
            let logged = &logged[..logged.len().min(budget)];
            if logged.is_empty() {
                drop((rows, log));
                tx.abort()?;
                return Ok((0, true));
            }
            for key in logged {
                if key[4] == TRANSFERRED && rows.remove(&key[5..])?.is_some() {
                    dropped += 1;
                }
                log.remove(key.as_slice())?;
            }
        }
        tx.commit()?;
        Ok((dropped, done))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip() {
        for row in [
            Tracked {
                created: 7,
                transferred: None,
                info: None,
            },
            Tracked {
                created: 840_000,
                transferred: Some(840_100),
                info: Some(vec![0x28, 0xb5, 0x2f, 0xfd]),
            },
        ] {
            assert_eq!(Tracked::decode(&row.encode()).unwrap(), row);
        }
    }
}
