use {
  super::*,
  crate::runes::{varint, Edict, Runestone, CLAIM_BIT},
};

use std::fs::File;

fn claim(id: u128) -> Option<u128> {
  (id & CLAIM_BIT != 0).then_some(id ^ CLAIM_BIT)
}

struct Allocation {
  balance: u128,
  deadline: Option<u32>,
  divisibility: u8,
  end: Option<u32>,
  id: u128,
  limit: Option<u128>,
  rune: Rune,
  spacers: u32,
  symbol: Option<char>,
}

#[derive(Default, Copy, Clone)]
pub(crate) struct RuneUpdate {
  pub(crate) burned: u128,
  pub(crate) mints: u64,
  pub(crate) supply: u128,
}

pub(super) struct RuneUpdater<'a, 'db, 'tx> {
  pub(super) height: u32,
  pub(super) id_to_entry: &'a mut Table<'db, 'tx, RuneIdValue, RuneEntryValue>,
  pub(super) inscription_id_to_sequence_number: &'a Table<'db, 'tx, InscriptionIdValue, u32>,
  pub(super) minimum: Rune,
  pub(super) outpoint_to_balances: &'a mut Table<'db, 'tx, &'static OutPointValue, &'static [u8]>,
  pub(super) rune_to_id: &'a mut Table<'db, 'tx, u128, RuneIdValue>,
  pub(super) runes: u64,
  pub(super) sequence_number_to_rune_id: &'a mut Table<'db, 'tx, u32, RuneIdValue>,
  pub(super) statistic_to_count: &'a mut Table<'db, 'tx, u64, u64>,
  pub(super) timestamp: u32,
  pub(super) transaction_id_to_rune: &'a mut Table<'db, 'tx, &'static TxidValue, u128>,
  pub(super) updates: HashMap<RuneId, RuneUpdate>,
  pub(super) first_in_block: bool,
  pub(super) chain: Chain,
}

impl<'a, 'db, 'tx> RuneUpdater<'a, 'db, 'tx> {
  fn write_to_file(
    &mut self,
    to_write: String,
    flush: bool,
  ) -> Result {
    lazy_static! {
      static ref RUNES_OUTPUT: Mutex<Option<File>> = Mutex::new(None);
    }
    let mut runes_output = RUNES_OUTPUT.lock().unwrap();
    if runes_output.as_ref().is_none() {
      let chain_folder: String = match self.chain { 
        Chain::Mainnet => String::from(""),
        Chain::Testnet => String::from("testnet3/"),
        Chain::Signet => String::from("signet/"),
        Chain::Regtest => String::from("regtest/"),
      };
      *runes_output = Some(File::options().append(true).open(format!("{chain_folder}runes_output.txt")).unwrap());
    }
    if to_write != "" {
      if self.first_in_block {
        println!("cmd;{0};block_start", self.height,);
        writeln!(runes_output.as_ref().unwrap(), "cmd;{0};block_start", self.height,)?;
      }
      self.first_in_block = false;

      writeln!(runes_output.as_ref().unwrap(), "{}", to_write)?;
    }
    if flush {
      (runes_output.as_ref().unwrap()).flush()?;
    }

    Ok(())
  }

  pub(super) fn end_block(
    &mut self,
  ) -> Result {
    let updates_clone = self.updates.clone();
    for (rune_id, update) in updates_clone {
      let mut entry = RuneEntry::load(
        self.id_to_entry
          .get(&rune_id.store())?
          .unwrap()
          .value(),
      );

      entry.burned += update.burned;
      entry.mints += update.mints;
      entry.supply += update.supply;

      self.id_to_entry.insert(&rune_id.store(), entry.store())?;
      self.write_to_file(format!("cmd;{0};id_to_entry_update;{1};{2};{3};{4}", self.height, rune_id, entry.burned, entry.mints, entry.supply), false)?;
    }

    if !self.first_in_block {
      println!("cmd;{0};block_end", self.height);
      self.write_to_file(format!("cmd;{0};block_end", self.height), true)?;
    }

    Ok(())
  }



  pub(super) fn index_runes(&mut self, index: usize, tx: &Transaction, txid: Txid) -> Result<()> {
    let runestone = Runestone::from_transaction(tx);

    // A mapping of rune ID to un-allocated balance of that rune
    let mut unallocated: HashMap<u128, u128> = HashMap::new();
    let mut tx_inputs: HashMap<OutPoint, Vec<(u128, u128)>> = HashMap::new(); // outpoint -> (id, amount)[]

    // Increment unallocated runes with the runes in this transaction's inputs
    for input in &tx.input {
      let mut removed = false;
      if let Some(guard) = self
        .outpoint_to_balances
        .remove(&input.previous_output.store())?
      {
        let buffer = guard.value();
        let mut i = 0;
        while i < buffer.len() {
          let (id, len) = varint::decode(&buffer[i..]);
          i += len;
          let (balance, len) = varint::decode(&buffer[i..]);
          i += len;
          *unallocated.entry(id).or_default() += balance;
          tx_inputs
            .entry(input.previous_output)
            .or_default()
            .push((id, balance));
        }
        removed = true;
      }

      if removed {
        self.write_to_file(format!("cmd;{0};outpoint_to_balances_remove;{1}", self.height, input.previous_output), false)?;
      }
    }
    let mut new_rune_allocations: HashMap<OutPoint, Vec<(u128, u128)>> = HashMap::new(); // outpoint -> (id, amount)[]
    let mut mints: HashMap<OutPoint, Vec<(u128, u128)>> = HashMap::new(); // outpoint -> (id, amount)[]
    let mut transfers: HashMap<OutPoint, Vec<(u128, u128)>> = HashMap::new(); // outpoint -> (id, amount)[]

    let burn = runestone
      .as_ref()
      .map(|runestone| runestone.burn)
      .unwrap_or_default();

    let default_output = runestone.as_ref().and_then(|runestone| {
      runestone
        .default_output
        .and_then(|default| usize::try_from(default).ok())
    });

    // A vector of allocated transaction output rune balances
    let mut allocated: Vec<HashMap<u128, u128>> = vec![HashMap::new(); tx.output.len()];

    if let Some(runestone) = runestone {
      // Determine if this runestone contains a valid issuance
      let mut allocation = match runestone.etching {
        Some(etching) => {
          if etching
            .rune
            .map(|rune| rune < self.minimum || rune.is_reserved())
            .unwrap_or_default()
            || etching
              .rune
              .and_then(|rune| self.rune_to_id.get(rune.0).transpose())
              .transpose()?
              .is_some()
          {
            None
          } else {
            let rune = if let Some(rune) = etching.rune {
              rune
            } else {
              let reserved_runes = self
                .statistic_to_count
                .get(&Statistic::ReservedRunes.into())?
                .map(|entry| entry.value())
                .unwrap_or_default();

              self
                .statistic_to_count
                .insert(&Statistic::ReservedRunes.into(), reserved_runes + 1)?;

              Rune::reserved(reserved_runes.into())
            };

            let (limit, term) = match (etching.limit, etching.term) {
              (None, Some(term)) => (Some(runes::MAX_LIMIT), Some(term)),
              (limit, term) => (limit, term),
            };

            // Construct an allocation, representing the new runes that may be
            // allocated. Beware: Because it would require constructing a block
            // with 2**16 + 1 transactions, there is no test that checks that
            // an eching in a transaction with an out-of-bounds index is
            // ignored.
            match u16::try_from(index) {
              Ok(index) => Some(Allocation {
                balance: if let Some(limit) = limit {
                  if term == Some(0) {
                    0
                  } else {
                    limit
                  }
                } else {
                  u128::max_value()
                },
                deadline: etching.deadline,
                divisibility: etching.divisibility,
                end: term.map(|term| term + self.height),
                id: u128::from(self.height) << 16 | u128::from(index),
                limit,
                rune,
                spacers: etching.spacers,
                symbol: etching.symbol,
              }),
              Err(_) => None,
            }
          }
        }
        None => None,
      };

      if !burn {
        let mut mintable: HashMap<u128, u128> = HashMap::new();

        let mut claims = runestone
          .edicts
          .iter()
          .filter_map(|edict| claim(edict.id))
          .collect::<Vec<u128>>();
        claims.sort();
        claims.dedup();
        for id in claims {
          if let Ok(key) = RuneId::try_from(id) {
            if let Some(entry) = self.id_to_entry.get(&key.store())? {
              let entry = RuneEntry::load(entry.value());
              if let Some(limit) = entry.limit {
                if let Some(end) = entry.end {
                  if self.height >= end {
                    continue;
                  }
                }
                if let Some(deadline) = entry.deadline {
                  if self.timestamp >= deadline {
                    continue;
                  }
                }
                mintable.insert(id, limit);
              }
            }
          }
        }

        let limits = mintable.clone();

        for Edict { id, amount, output } in runestone.edicts {
          let Ok(output) = usize::try_from(output) else {
            continue;
          };

          // Skip edicts not referring to valid outputs
          if output > tx.output.len() {
            continue;
          }

          let (balance, id, output_type) = if id == 0 {
            // If this edict allocates new issuance runes, skip it
            // if no issuance was present, or if the issuance was invalid.
            // Additionally, replace ID 0 with the newly assigned ID, and
            // get the unallocated balance of the issuance.
            match allocation.as_mut() {
              Some(Allocation { balance, id, .. }) => (balance, *id, 0),
              None => continue,
            }
          } else if let Some(claim) = claim(id) {
            match mintable.get_mut(&claim) {
              Some(balance) => (balance, claim, 1),
              None => continue,
            }
          } else {
            // Get the unallocated balance of the given ID
            match unallocated.get_mut(&id) {
              Some(balance) => (balance, id, 2),
              None => continue,
            }
          };

          let mut allocate = |balance: &mut u128, amount: u128, output: usize| {
            if amount > 0 {
              *balance -= amount;
              *allocated[output].entry(id).or_default() += amount;
            }
          };
          let mut save_to_maps = |output: usize, amount: u128| {
            let outpoint = OutPoint {
              txid,
              vout: u32::try_from(output).unwrap(),
            };
            
            if output_type == 0 {
              // if outpoint already in new_rune_allocations, check ids and if match found, add amount to existing amount
              if let Some((_, amt)) = new_rune_allocations.get_mut(&outpoint).and_then(|v| v.iter_mut().find(|(id_, _)| *id_ == id)) {
                *amt += amount;
              } else {
                new_rune_allocations.entry(outpoint).or_default().push((id, amount));
              }
            } else if output_type == 1 {
              // if outpoint already in mints, check ids and if match found, add amount to existing amount
              if let Some((_, amt)) = mints.get_mut(&outpoint).and_then(|v| v.iter_mut().find(|(id_, _)| *id_ == id)) {
                *amt += amount;
              } else {
                mints.entry(outpoint).or_default().push((id, amount));
              }
            } else if output_type == 2 {
              // if outpoint already in transfers, check ids and if match found, add amount to existing amount
              if let Some((_, amt)) = transfers.get_mut(&outpoint).and_then(|v| v.iter_mut().find(|(id_, _)| *id_ == id)) {
                *amt += amount;
              } else {
                transfers.entry(outpoint).or_default().push((id, amount));
              }
            }
          };

          if output == tx.output.len() {
            // find non-OP_RETURN outputs
            let destinations = tx
              .output
              .iter()
              .enumerate()
              .filter_map(|(output, tx_out)| {
                (!tx_out.script_pubkey.is_op_return()).then_some(output)
              })
              .collect::<Vec<usize>>();

            if amount == 0 {
              // if amount is zero, divide balance between eligible outputs
              let amount = *balance / destinations.len() as u128;
              let remainder = usize::try_from(*balance % destinations.len() as u128).unwrap();

              for (i, output) in destinations.iter().enumerate() {
                let amt = if i < remainder { amount + 1 } else { amount };
                allocate(
                  balance,
                  amt,
                  *output,
                );

                save_to_maps(*output, amt);
              }
            } else {
              // if amount is non-zero, distribute amount to eligible outputs
              for output in destinations {
                let amt = amount.min(*balance);
                allocate(balance, amt, output);
                save_to_maps(output, amt);
              }
            }
          } else {
            // Get the allocatable amount
            let amount = if amount == 0 {
              *balance
            } else {
              amount.min(*balance)
            };

            allocate(balance, amount, output);
            save_to_maps(output, amount);
          }
        }

        // increment entries with minted runes
        for (id, amount) in mintable {
          let minted = limits[&id] - amount;
          if minted > 0 {
            let update = self
              .updates
              .entry(RuneId::try_from(id).unwrap())
              .or_default();
            update.mints += 1;
            update.supply += minted;
          }
        }
      }

      if let Some(Allocation {
        balance,
        deadline,
        divisibility,
        end,
        id,
        limit,
        rune,
        spacers,
        symbol,
      }) = allocation
      {
        let id = RuneId::try_from(id).unwrap();
        self.rune_to_id.insert(rune.0, id.store())?;
        // self.write_to_file(format!("cmd;{0};rune_to_id_insert;{1};{2}", self.height, rune, id), false)?;
        self.transaction_id_to_rune.insert(&txid.store(), rune.0)?;
        // self.write_to_file(format!("cmd;{0};transaction_id_to_rune_insert;{1};{2}", self.height, txid, rune), false)?;
        let number = self.runes;
        self.runes += 1;
        self
          .statistic_to_count
          .insert(&Statistic::Runes.into(), self.runes)?;
        let rune_entry = RuneEntry {
          burned: 0,
          deadline: deadline.and_then(|deadline| (!burn).then_some(deadline)),
          divisibility,
          etching: txid,
          mints: 0,
          number,
          rune,
          spacers,
          supply: if let Some(limit) = limit {
            if end == Some(self.height) {
              0
            } else {
              limit
            }
          } else {
            u128::max_value()
          } - balance,
          end: end.and_then(|end| (!burn).then_some(end)),
          symbol,
          limit: limit.and_then(|limit| (!burn).then_some(limit)),
          timestamp: self.timestamp,
        };
        self.id_to_entry.insert(
          id.store(),
          rune_entry.store(),
        )?;
        let mut buff_for_symbol = [0; 4];
        if symbol.is_some() {
          symbol.unwrap().encode_utf8(&mut buff_for_symbol);
        }
        self.write_to_file(format!("cmd;{0};id_to_entry_insert;{1};{2};{3};{4};{5};{6};{7};{8};{9};{10};{11};{12};{13};{14};{15}", 
                self.height, id, rune_entry.rune.0, rune_entry.burned, rune_entry.deadline.map_or(String::from("null"), |v| v.to_string()), rune_entry.divisibility, 
                rune_entry.etching, rune_entry.mints, rune_entry.number, rune_entry.rune, rune_entry.spacers, rune_entry.supply, 
                rune_entry.end.map_or(String::from("null"), |v| v.to_string()), rune_entry.symbol.map_or(String::from("null"), |_| hex::encode(buff_for_symbol)), 
                rune_entry.limit.map_or(String::from("null"), |v| v.to_string()), rune_entry.timestamp), false)?;

        let inscription_id = InscriptionId { txid, index: 0 };

        if let Some(sequence_number) = self
          .inscription_id_to_sequence_number
          .get(&inscription_id.store())?
        {
          self
            .sequence_number_to_rune_id
            .insert(sequence_number.value(), id.store())?;
        }
      }
    }

    let mut burned: HashMap<u128, u128> = HashMap::new(); // NOTE: use this for tracking burns

    if burn {
      for (id, balance) in unallocated {
        *burned.entry(id).or_default() += balance;
      }
    } else {
      // assign all un-allocated runes to the default output, or the first non
      // OP_RETURN output if there is no default, or if the default output is
      // too large
      if let Some(vout) = default_output
        .filter(|vout| *vout < allocated.len())
        .or_else(|| {
          tx.output
            .iter()
            .enumerate()
            .find(|(_vout, tx_out)| !tx_out.script_pubkey.is_op_return())
            .map(|(vout, _tx_out)| vout)
        })
      {
        for (id, balance) in unallocated {
          if balance > 0 {
            *allocated[vout].entry(id).or_default() += balance;

            let outpoint = OutPoint {
              txid,
              vout: vout.try_into().unwrap(),
            };
            if let Some((_, amt)) = transfers.get_mut(&outpoint).and_then(|v| v.iter_mut().find(|(id_, _)| *id_ == id)) {
              *amt += balance;
            } else {
              transfers.entry(outpoint).or_default().push((id, balance));
            }
          }
        }
      } else {
        for (id, balance) in unallocated {
          if balance > 0 {
            *burned.entry(id).or_default() += balance;
          }
        }
      }
    }

    // update outpoint balances
    let mut buffer: Vec<u8> = Vec::new();
    for (vout, balances) in allocated.into_iter().enumerate() {
      if balances.is_empty() {
        continue;
      }

      // increment burned balances
      if tx.output[vout].script_pubkey.is_op_return() {
        for (id, balance) in &balances {
          *burned.entry(*id).or_default() += balance;
        }
        continue;
      }

      buffer.clear();

      let mut balances = balances.into_iter().collect::<Vec<(u128, u128)>>();

      // Sort balances by id so tests can assert balances in a fixed order
      balances.sort();

      let mut balances_str = String::new();
      for (id, balance) in balances {
        varint::encode_to_vec(id, &mut buffer);
        varint::encode_to_vec(balance, &mut buffer);

        balances_str += &format!("{0}-{1}-{2},", id, RuneId::try_from(id)?, balance);
      }

      let outpoint = OutPoint {
        txid,
        vout: vout.try_into().unwrap(),
      };
      self.outpoint_to_balances.insert(
        &outpoint.store(),
        buffer.as_slice(),
      )?;
      let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());
      self.write_to_file(format!("cmd;{0};outpoint_to_balances_insert;{1};{2};{3}", self.height, scriptpubkeyhex, outpoint, balances_str), false)?;
    }

    // use tx_inputs, new_rune_allocations, mints, transfers, and burned to get event string
    // cmd;<height>;tx_events_input;<txid>;<outpoint>;<id>;<amount>
    // cmd;<height>;tx_events_new_rune_allocation;<txid>;<outpoint>;<id>;<amount>
    // cmd;<height>;tx_events_mint;<txid>;<outpoint>;<id>;<amount>
    // cmd;<height>;tx_events_transfer;<txid>;<outpoint>;<id>;<amount>
    // cmd;<height>;tx_events_burn;<txid>;<id>;<amount>
    for (outpoint, balances) in tx_inputs {
      for (id, amount) in balances {
        let rune_id = RuneId::try_from(id).unwrap();
        self.write_to_file(format!("cmd;{0};tx_events_input;{1};{2};{3};{4}", self.height, txid, outpoint, rune_id, amount), false)?;
      }
    }
    for (outpoint, balances) in new_rune_allocations {
      for (id, amount) in balances {
        let rune_id = RuneId::try_from(id).unwrap();
        let vout = usize::try_from(outpoint.vout).unwrap();
        let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());
        self.write_to_file(format!("cmd;{0};tx_events_new_rune_allocation;{1};{2};{3};{4};{5}", self.height, txid, outpoint, rune_id, amount, scriptpubkeyhex), false)?;
      }
    }
    for (outpoint, balances) in mints {
      for (id, amount) in balances {
        let rune_id = RuneId::try_from(id).unwrap();
        let vout = usize::try_from(outpoint.vout).unwrap();
        let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());
        self.write_to_file(format!("cmd;{0};tx_events_mint;{1};{2};{3};{4};{5}", self.height, txid, outpoint, rune_id, amount, scriptpubkeyhex), false)?;
      }
    }
    for (outpoint, balances) in transfers {
      for (id, amount) in balances {
        let rune_id = RuneId::try_from(id).unwrap();
        let vout = usize::try_from(outpoint.vout).unwrap();
        let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());
        self.write_to_file(format!("cmd;{0};tx_events_transfer;{1};{2};{3};{4};{5}", self.height, txid, outpoint, rune_id, amount, scriptpubkeyhex), false)?;
      }
    }

    // increment entries with burned runes
    for (id, amount) in burned {
      let rune_id = RuneId::try_from(id).unwrap();
      self
        .updates
        .entry(rune_id)
        .or_default()
        .burned += amount;
      self.write_to_file(format!("cmd;{0};tx_events_burn;{1};{2};{3}", self.height, txid, rune_id, amount), false)?;
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn claim_from_id() {
    assert_eq!(claim(1), None);
    assert_eq!(claim(1 | CLAIM_BIT), Some(1));
  }
}
