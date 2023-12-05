use {super::*, inscription::Curse};

use std::fs::File;
use serde_json::Value;
use hex;

#[derive(Debug, Clone)]
pub(super) struct Flotsam {
  inscription_id: InscriptionId,
  offset: u64,
  origin: Origin,
  tx_option: Option<Transaction>,
}

// tracking first 2 transfers is enough for brc-20 metaprotocol
const INDEX_TX_LIMIT : i64 = 2;

#[derive(Debug, Clone)]
enum Origin {
  New {
    cursed: bool,
    fee: u64,
    parent: Option<InscriptionId>,
    unbound: bool,
  },
  Old {
    old_satpoint: SatPoint,
  },
}

pub(super) struct InscriptionUpdater<'a, 'db, 'tx> {
  flotsam: Vec<Flotsam>,
  height: u64,
  id_to_children:
    &'a mut MultimapTable<'db, 'tx, &'static InscriptionIdValue, &'static InscriptionIdValue>,
  id_to_satpoint: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, &'static SatPointValue>,
  value_receiver: &'a mut Receiver<u64>,
  id_to_entry: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, InscriptionEntryValue>,
  pub(super) lost_sats: u64,
  pub(super) next_cursed_number: i64,
  pub(super) next_number: i64,
  number_to_id: &'a mut Table<'db, 'tx, i64, &'static InscriptionIdValue>,
  id_to_txcnt: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, i64>,
  outpoint_to_value: &'a mut Table<'db, 'tx, &'static OutPointValue, u64>,
  reward: u64,
  reinscription_id_to_seq_num: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, u64>,
  sat_to_inscription_id: &'a mut MultimapTable<'db, 'tx, u64, &'static InscriptionIdValue>,
  satpoint_to_id:
    &'a mut MultimapTable<'db, 'tx, &'static SatPointValue, &'static InscriptionIdValue>,
  timestamp: u32,
  pub(super) unbound_inscriptions: u64,
  value_cache: &'a mut HashMap<OutPoint, u64>,
  first_in_block: bool,
}

impl<'a, 'db, 'tx> InscriptionUpdater<'a, 'db, 'tx> {
  pub(super) fn new(
    height: u64,
    id_to_children: &'a mut MultimapTable<
      'db,
      'tx,
      &'static InscriptionIdValue,
      &'static InscriptionIdValue,
    >,
    id_to_satpoint: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, &'static SatPointValue>,
    value_receiver: &'a mut Receiver<u64>,
    id_to_entry: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, InscriptionEntryValue>,
    lost_sats: u64,
    number_to_id: &'a mut Table<'db, 'tx, i64, &'static InscriptionIdValue>,
    id_to_txcnt: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, i64>,
    outpoint_to_value: &'a mut Table<'db, 'tx, &'static OutPointValue, u64>,
    reinscription_id_to_seq_num: &'a mut Table<'db, 'tx, &'static InscriptionIdValue, u64>,
    sat_to_inscription_id: &'a mut MultimapTable<'db, 'tx, u64, &'static InscriptionIdValue>,
    satpoint_to_id: &'a mut MultimapTable<
      'db,
      'tx,
      &'static SatPointValue,
      &'static InscriptionIdValue,
    >,
    timestamp: u32,
    unbound_inscriptions: u64,
    value_cache: &'a mut HashMap<OutPoint, u64>,
  ) -> Result<Self> {
    let next_cursed_number = number_to_id
      .iter()?
      .next()
      .and_then(|result| result.ok())
      .map(|(number, _id)| number.value() - 1)
      .unwrap_or(-1);

    let next_number = number_to_id
      .iter()?
      .next_back()
      .and_then(|result| result.ok())
      .map(|(number, _id)| number.value() + 1)
      .unwrap_or(0);

    Ok(Self {
      flotsam: Vec::new(),
      height,
      id_to_children,
      id_to_satpoint,
      value_receiver,
      id_to_entry,
      lost_sats,
      next_cursed_number,
      next_number,
      number_to_id,
      id_to_txcnt,
      outpoint_to_value,
      reward: Height(height).subsidy(),
      reinscription_id_to_seq_num,
      sat_to_inscription_id,
      satpoint_to_id,
      timestamp,
      unbound_inscriptions,
      value_cache,
      first_in_block: true,
    })
  }

  pub(super) fn index_transaction_inscriptions(
    &mut self,
    tx: &Transaction,
    txid: Txid,
    input_sat_ranges: Option<&VecDeque<(u64, u64)>>,
  ) -> Result {
    let mut new_inscriptions = Inscription::from_transaction(tx).into_iter().peekable();
    let mut floating_inscriptions = Vec::new();
    let mut inscribed_offsets = BTreeMap::new();
    let mut total_input_value = 0;
    let mut id_counter = 0;

    for (input_index, tx_in) in tx.input.iter().enumerate() {
      // skip subsidy since no inscriptions possible
      if tx_in.previous_output.is_null() {
        total_input_value += Height(self.height).subsidy();
        continue;
      }

      // find existing inscriptions on input (transfers of inscriptions)
      for (old_satpoint, inscription_id) in Index::inscriptions_on_output_ordered(
        self.reinscription_id_to_seq_num,
        self.satpoint_to_id,
        tx_in.previous_output,
      )? {
        let offset = total_input_value + old_satpoint.offset;
        floating_inscriptions.push(Flotsam {
          offset,
          inscription_id,
          origin: Origin::Old { old_satpoint },
          tx_option: Some(tx.clone()),
        });

        inscribed_offsets
          .entry(offset)
          .and_modify(|(_id, count)| *count += 1)
          .or_insert((inscription_id, 0));
      }

      let offset = total_input_value;

      // multi-level cache for UTXO set to get to the input amount
      let current_input_value = if let Some(value) = self.value_cache.remove(&tx_in.previous_output)
      {
        value
      } else if let Some(value) = self
        .outpoint_to_value
        .remove(&tx_in.previous_output.store())?
      {
        value.value()
      } else {
        self.value_receiver.blocking_recv().ok_or_else(|| {
          anyhow!(
            "failed to get transaction for {}",
            tx_in.previous_output.txid
          )
        })?
      };

      total_input_value += current_input_value;

      // go through all inscriptions in this input
      while let Some(inscription) = new_inscriptions.peek() {
        if inscription.tx_in_index != u32::try_from(input_index).unwrap() {
          break;
        }

        let inscription_id = InscriptionId {
          txid,
          index: id_counter,
        };

        let curse = if inscription.inscription.unrecognized_even_field {
          Some(Curse::UnrecognizedEvenField)
        } else if inscription.tx_in_index != 0 {
          Some(Curse::NotInFirstInput)
        } else if inscription.tx_in_offset != 0 {
          Some(Curse::NotAtOffsetZero)
        } else if inscribed_offsets.contains_key(&offset) {
          let seq_num = self.reinscription_id_to_seq_num.len()?;

          let sat = Self::calculate_sat(input_sat_ranges, offset);
          log::info!("processing reinscription {inscription_id} on sat {:?}: sequence number {seq_num}, inscribed offsets {:?}", sat, inscribed_offsets);

          // if reinscription track its ordering
          self
            .reinscription_id_to_seq_num
            .insert(&inscription_id.store(), seq_num)?;

          Some(Curse::Reinscription)
        } else {
          None
        };

        if curse.is_some() {
          log::info!("found cursed inscription {inscription_id}: {:?}", curse);
        }

        let cursed = if let Some(Curse::Reinscription) = curse {
          let first_reinscription = inscribed_offsets
            .get(&offset)
            .map(|(_id, count)| count == &0)
            .unwrap_or(false);

          let initial_inscription_is_cursed = inscribed_offsets
            .get(&offset)
            .and_then(|(inscription_id, _count)| {
              match self.id_to_entry.get(&inscription_id.store()) {
                Ok(option) => option.map(|entry| {
                  let loaded_entry = InscriptionEntry::load(entry.value());
                  loaded_entry.number < 0
                }),
                Err(_) => None,
              }
            })
            .unwrap_or(false);

          log::info!("{inscription_id}: is first reinscription: {first_reinscription}, initial inscription is cursed: {initial_inscription_is_cursed}");

          !(initial_inscription_is_cursed && first_reinscription)
        } else {
          curse.is_some()
        };

        let unbound = current_input_value == 0
          || inscription.tx_in_offset != 0
          || curse == Some(Curse::UnrecognizedEvenField);

        if curse.is_some() || unbound {
          log::info!(
            "indexing inscription {inscription_id} with curse {:?} as cursed {} and unbound {}",
            curse,
            cursed,
            unbound
          );
        }

        floating_inscriptions.push(Flotsam {
          inscription_id,
          offset,
          origin: Origin::New {
            cursed,
            fee: 0,
            parent: inscription.inscription.parent(),
            unbound,
          },
          tx_option: Some(tx.clone()),
        });

        new_inscriptions.next();
        id_counter += 1;
      }
    }

    let potential_parents = floating_inscriptions
      .iter()
      .map(|flotsam| flotsam.inscription_id)
      .collect::<HashSet<InscriptionId>>();

    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New { parent, .. },
        ..
      } = flotsam
      {
        if let Some(purported_parent) = parent {
          if !potential_parents.contains(purported_parent) {
            *parent = None;
          }
        }
      }
    }

    // still have to normalize over inscription size
    let total_output_value = tx.output.iter().map(|txout| txout.value).sum::<u64>();
    let mut floating_inscriptions = floating_inscriptions
      .into_iter()
      .map(|flotsam| {
        if let Flotsam {
          inscription_id,
          offset,
          origin:
            Origin::New {
              cursed,
              fee: _,
              parent,
              unbound,
            },
          tx_option,
        } = flotsam
        {
          Flotsam {
            inscription_id,
            offset,
            origin: Origin::New {
              fee: (total_input_value - total_output_value) / u64::from(id_counter),
              cursed,
              parent,
              unbound,
            },
            tx_option,
          }
        } else {
          flotsam
        }
      })
      .collect::<Vec<Flotsam>>();

    let is_coinbase = tx
      .input
      .first()
      .map(|tx_in| tx_in.previous_output.is_null())
      .unwrap_or_default();

    let own_inscription_cnt = floating_inscriptions.len();
    if is_coinbase {
      floating_inscriptions.append(&mut self.flotsam);
    }

    floating_inscriptions.sort_by_key(|flotsam| flotsam.offset);
    let mut inscriptions = floating_inscriptions.into_iter().peekable();

    let mut output_value = 0;
    let mut inscription_idx = 0;
    for (vout, tx_out) in tx.output.iter().enumerate() {
      let end = output_value + tx_out.value;

      while let Some(flotsam) = inscriptions.peek() {
        if flotsam.offset >= end {
          break;
        }

        let sent_to_coinbase = inscription_idx >= own_inscription_cnt;
        inscription_idx += 1;

        let new_satpoint = SatPoint {
          outpoint: OutPoint {
            txid,
            vout: vout.try_into().unwrap(),
          },
          offset: flotsam.offset - output_value,
        };

        let tx = flotsam.tx_option.clone().unwrap();
        self.update_inscription_location(
          Some(&tx),
          Some(&tx_out.script_pubkey),
          input_sat_ranges,
          inscriptions.next().unwrap(),
          new_satpoint,
          sent_to_coinbase,
        )?;
      }

      output_value = end;

      self.value_cache.insert(
        OutPoint {
          vout: vout.try_into().unwrap(),
          txid,
        },
        tx_out.value,
      );
    }

    if is_coinbase {
      for flotsam in inscriptions {
        let new_satpoint = SatPoint {
          outpoint: OutPoint::null(),
          offset: self.lost_sats + flotsam.offset - output_value,
        };
        let tx = flotsam.tx_option.clone().unwrap();
        self.update_inscription_location(Some(&tx), None, input_sat_ranges, flotsam, new_satpoint, true)?;
      }
      self.lost_sats += self.reward - output_value;
      Ok(())
    } else {
      for flotsam in inscriptions {
        self.flotsam.push(Flotsam {
          offset: self.reward + flotsam.offset - output_value,
          ..flotsam
        });

        // ord indexes sent as fee transfers at the end of the block but it would make more sense if they were indexed as soon as they are sent
        self.write_to_file(format!("cmd;{0};insert;early_transfer_sent_as_fee;{1}", self.height, flotsam.inscription_id), true)?;
      }
      self.reward += total_input_value - output_value;
      Ok(())
    }
  }

  fn calculate_sat(
    input_sat_ranges: Option<&VecDeque<(u64, u64)>>,
    input_offset: u64,
  ) -> Option<Sat> {
    let mut sat = None;
    if let Some(input_sat_ranges) = input_sat_ranges {
      let mut offset = 0;
      for (start, end) in input_sat_ranges {
        let size = end - start;
        if offset + size > input_offset {
          let n = start + input_offset - offset;
          sat = Some(Sat(n));
          break;
        }
        offset += size;
      }
    }
    sat
  }

  fn is_json(inscription_content_option: &Option<Vec<u8>>) -> bool {
    if inscription_content_option.is_none() { return false; }
    let inscription_content = inscription_content_option.as_ref().unwrap();

    return serde_json::from_slice::<Value>(&inscription_content).is_ok();
  }

  fn is_text(inscription_content_type_option: &Option<Vec<u8>>) -> bool {
    if inscription_content_type_option.is_none() { return false; }
    
    let inscription_content_type = inscription_content_type_option.as_ref().unwrap();
    let inscription_content_type_str = std::str::from_utf8(&inscription_content_type).unwrap_or("");
    return inscription_content_type_str == "text/plain" || inscription_content_type_str.starts_with("text/plain;") || 
            inscription_content_type_str == "application/json" || inscription_content_type_str.starts_with("application/json;"); // NOTE: added application/json for JSON5 etc.
  }

  fn write_to_file(
    &mut self,
    to_write: String,
    flush: bool,
  ) -> Result {
    lazy_static! {
      static ref LOG_FILE: File = File::options().append(true).open("log_file.txt").unwrap();
    }
    if to_write != "" {
      if self.first_in_block {
        println!("cmd;{0};block_start", self.height,);
        writeln!(&*LOG_FILE, "cmd;{0};block_start", self.height,)?;
      }
      self.first_in_block = false;

      writeln!(&*LOG_FILE, "{}", to_write)?;
    }
    if flush {
      (&*LOG_FILE).flush()?;
    }

    Ok(())
  }

  pub(super) fn end_block(
    &mut self,
  ) -> Result {
    if !self.first_in_block {
      println!("cmd;{0};block_end", self.height);
      self.write_to_file(format!("cmd;{0};block_end", self.height), true)?;
    }

    Ok(())
  }

  fn update_inscription_location(
    &mut self,
    tx_option: Option<&Transaction>,
    new_script_pubkey: Option<&ScriptBuf>,
    input_sat_ranges: Option<&VecDeque<(u64, u64)>>,
    flotsam: Flotsam,
    new_satpoint: SatPoint,
    send_to_coinbase: bool,
  ) -> Result {
    let tx = tx_option.unwrap();
    let inscription_id = flotsam.inscription_id.store();
    let txcnt_of_inscr: i64 = self.id_to_txcnt.get(&inscription_id)?
        .map(|txcnt| txcnt.value())
        .unwrap_or(0) + 1;
    if txcnt_of_inscr <= INDEX_TX_LIMIT { // only track first two transactions
      self.id_to_txcnt.insert(&inscription_id, &txcnt_of_inscr)?;
    }

    let unbound = match flotsam.origin {
      Origin::Old { old_satpoint } => {
        self.satpoint_to_id.remove_all(&old_satpoint.store())?;

        // get number and is_json from id_to_entry
        let entry = self.id_to_entry.get(&inscription_id)?;
        let entry = entry
          .map(|entry| InscriptionEntry::load(entry.value()))
          .unwrap();
        let number = entry.number;
        let is_json_or_text = entry.is_json_or_text;
        if number >= 0 && is_json_or_text && txcnt_of_inscr <= INDEX_TX_LIMIT { // only track non-cursed and first two transactions
          self.write_to_file(format!("cmd;{0};insert;transfer;{1};{old_satpoint};{new_satpoint};{send_to_coinbase};{2}", 
                    self.height, flotsam.inscription_id, 
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes())), false)?;
        }

        false
      }
      Origin::New {
        cursed,
        fee,
        parent,
        unbound,
      } => {
        let number = if cursed {
          let next_cursed_number = self.next_cursed_number;
          self.next_cursed_number -= 1;

          next_cursed_number
        } else {
          let next_number = self.next_number;
          self.next_number += 1;

          next_number
        };

        self.number_to_id.insert(number, &inscription_id)?;

        let inscription = Inscription::from_transaction(&tx)
            .get(flotsam.inscription_id.index as usize)
            .unwrap()
            .inscription.clone();
        let inscription_content = inscription.body;
        let inscription_content_type = inscription.content_type;
        let is_json = Self::is_json(&inscription_content);
        let is_text = Self::is_text(&inscription_content_type);
        let is_json_or_text = is_json || is_text;

        if !unbound && !cursed && is_json_or_text {
          self.write_to_file(format!("cmd;{0};insert;number_to_id;{1};{2}", self.height, number, flotsam.inscription_id), false)?;
          // write content as minified json
          if is_json {
            let inscription_content_json = serde_json::from_slice::<Value>(&(inscription_content.unwrap())).unwrap();
            let inscription_content_json_str = serde_json::to_string(&inscription_content_json).unwrap();
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4}", 
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_content_json_str), false)?;
          } else {
            let inscription_content_hex_str = hex::encode(inscription_content.unwrap_or(Vec::new()));
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4}", 
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_content_hex_str), false)?;
          }
        }

        let sat = if unbound {
          None
        } else {
          Self::calculate_sat(input_sat_ranges, flotsam.offset)
        };

        if let Some(Sat(n)) = sat {
          self.sat_to_inscription_id.insert(&n, &inscription_id)?;
        }

        self.id_to_entry.insert(
          &inscription_id,
          &InscriptionEntry {
            fee,
            height: self.height,
            number,
            parent,
            sat,
            timestamp: self.timestamp,
            is_json_or_text,
          }
          .store(),
        )?;

        if let Some(parent) = parent {
          self
            .id_to_children
            .insert(&parent.store(), &inscription_id)?;
        }

        if !unbound && !cursed && is_json_or_text {
          self.write_to_file(format!("cmd;{0};insert;transfer;{1};;{new_satpoint};{send_to_coinbase};{2}", 
                    self.height, flotsam.inscription_id, 
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes())), false)?;
        }

        unbound
      }
    };

    let satpoint = if unbound {
      let new_unbound_satpoint = SatPoint {
        outpoint: unbound_outpoint(),
        offset: self.unbound_inscriptions,
      };
      self.unbound_inscriptions += 1;
      new_unbound_satpoint.store()
    } else {
      new_satpoint.store()
    };

    self.satpoint_to_id.insert(&satpoint, &inscription_id)?;
    self.id_to_satpoint.insert(&inscription_id, &satpoint)?;

    self.write_to_file("".to_string(), true)?;

    Ok(())
  }
}
