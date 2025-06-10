use super::*;

use rocksdb::WriteOptions;
use serde_json::Value;

#[derive(Debug, PartialEq, Copy, Clone)]
enum Curse {
  DuplicateField,
  IncompleteField,
  NotAtOffsetZero,
  NotInFirstInput,
  Pointer,
  Pushnum,
  Reinscription,
  Stutter,
  UnrecognizedEvenField,
}

#[derive(Debug, Clone)]
pub(super) struct Flotsam<'a> {
  inscription_id: InscriptionId,
  offset: u64,
  origin: Origin,
  tx_option: Option<&'a Transaction>,
}

lazy_static! {
  pub static ref TX_LIMITS: HashMap<String, i16> = {
      let mut m = HashMap::<String, i16>::new();
      m.insert("default".into(), 2);
      m
  };
}

#[derive(Debug, Clone)]
enum Origin {
  New {
    cursed: bool,
    cursed_for_brc20: bool,
    fee: u64,
    parents: Vec<InscriptionId>,
    reinscription: bool,
    unbound: bool,
    vindicated: bool,
  },
  Old {
    sequence_number: u32,
    old_satpoint: SatPoint,
  },
}

pub(super) struct InscriptionUpdater<'a> {
  pub(super) blessed_inscription_count: u64,
  pub(super) cursed_inscription_count: u64,
  pub(super) flotsam: Vec<Flotsam<'a>>,
  pub(super) height: u32,
  pub(super) db: &'a DB,
  pub(super) id_to_sequence_number: &'a ColumnFamily,
  pub(super) inscription_number_to_sequence_number: &'a ColumnFamily,
  pub(super) id_to_txcnt: &'a ColumnFamily,
  pub(super) next_sequence_number: u32,
  pub(super) reward: u64,
  pub(super) sequence_number_to_entry: &'a ColumnFamily,
  pub(super) ord_transfers: &'a ColumnFamily,
  pub(super) ord_inscription_info: &'a ColumnFamily,
  pub(super) transfer_idx: u32,
  pub(super) early_transfer_info: HashMap<InscriptionId, u32>,
  pub(super) write_options: &'a WriteOptions,
}

impl<'a> InscriptionUpdater<'a> {
  pub(super) fn index_inscriptions(
    &mut self,
    tx: &'a Transaction,
    txid: Txid,
    input_utxo_entries: &[ParsedUtxoEntry],
    output_utxo_entries: &mut [UtxoEntryBuf],
    utxo_cache: &mut HashMap<OutPoint, UtxoEntryBuf>,
    index: &Index,
    input_sat_ranges: Option<&Vec<&[u8]>>,
  ) -> Result {
    let mut floating_inscriptions = Vec::new();
    let mut id_counter = 0;
    let mut inscribed_offsets = BTreeMap::new();
    let jubilant = self.height >= index.settings.chain().jubilee_height();
    let mut total_input_value = 0;
    let total_output_value = tx
      .output
      .iter()
      .map(|txout| txout.value.to_sat())
      .sum::<u64>();

    let envelopes = ParsedEnvelope::from_transaction(tx);
    let mut envelopes = envelopes.into_iter().peekable();

    for (input_index, txin) in tx.input.iter().enumerate() {
      // skip subsidy since no inscriptions possible
      if txin.previous_output.is_null() {
        total_input_value += Height(self.height).subsidy();
        continue;
      }

      let mut transferred_inscriptions = input_utxo_entries[input_index].parse_inscriptions();

      transferred_inscriptions.sort_by_key(|(sequence_number, _)| *sequence_number);

      for (sequence_number, old_satpoint_offset) in transferred_inscriptions {
        let old_satpoint = SatPoint {
          outpoint: txin.previous_output,
          offset: old_satpoint_offset,
        };

        let inscription_id = InscriptionEntry::load(
          self
            .db
            .get_cf(self.sequence_number_to_entry, sequence_number.to_be_bytes())?
            .unwrap(),
        )
        .id;

        let offset = total_input_value + old_satpoint_offset;
        floating_inscriptions.push(Flotsam {
          offset,
          inscription_id,
          origin: Origin::Old {
            sequence_number,
            old_satpoint,
          },
          tx_option: Some(&tx),
        });

        inscribed_offsets
          .entry(offset)
          .or_insert((inscription_id, 0))
          .1 += 1;
      }

      let offset = total_input_value;

      let input_value = input_utxo_entries[input_index].total_value();
      total_input_value += input_value;

      // go through all inscriptions in this input
      while let Some(inscription) = envelopes.peek() {
        if inscription.input != u32::try_from(input_index).unwrap() {
          break;
        }

        let inscription_id = InscriptionId {
          txid,
          index: id_counter,
        };

        let curse = if inscription.payload.unrecognized_even_field {
          Some(Curse::UnrecognizedEvenField)
        } else if inscription.payload.duplicate_field {
          Some(Curse::DuplicateField)
        } else if inscription.payload.incomplete_field {
          Some(Curse::IncompleteField)
        } else if inscription.input != 0 {
          Some(Curse::NotInFirstInput)
        } else if inscription.offset != 0 {
          Some(Curse::NotAtOffsetZero)
        } else if inscription.payload.pointer.is_some() {
          Some(Curse::Pointer)
        } else if inscription.pushnum {
          Some(Curse::Pushnum)
        } else if inscription.stutter {
          Some(Curse::Stutter)
        } else if let Some((id, count)) = inscribed_offsets.get(&offset) {
          if *count > 1 {
            Some(Curse::Reinscription)
          } else {
            let initial_inscription_sequence_number =
              u32::from_be_bytes(self.db.get_cf(self.id_to_sequence_number, id.store())?.unwrap().try_into().unwrap());

            let entry = InscriptionEntry::load(
              self
                .db
                .get_cf(self.sequence_number_to_entry, initial_inscription_sequence_number.to_be_bytes())?
                .unwrap()
            );

            let initial_inscription_was_cursed_or_vindicated =
              entry.inscription_number < 0 || Charm::Vindicated.is_set(entry.charms);

            if initial_inscription_was_cursed_or_vindicated {
              None
            } else {
              Some(Curse::Reinscription)
            }
          }
        } else {
          None
        };

        let offset = inscription
          .payload
          .pointer()
          .filter(|&pointer| pointer < total_output_value)
          .unwrap_or(offset);

        floating_inscriptions.push(Flotsam {
          inscription_id,
          offset,
          origin: Origin::New {
            cursed: curse.is_some() && !jubilant,
            cursed_for_brc20: curse.is_some(),
            fee: 0,
            parents: inscription.payload.parents(),
            reinscription: inscribed_offsets.contains_key(&offset),
            unbound: input_value == 0
              || curse == Some(Curse::UnrecognizedEvenField)
              || inscription.payload.unrecognized_even_field,
            vindicated: curse.is_some() && jubilant,
          },
          tx_option: Some(&tx),
        });

        inscribed_offsets
          .entry(offset)
          .or_insert((inscription_id, 0))
          .1 += 1;

        envelopes.next();
        id_counter += 1;
      }
    }

    let potential_parents = floating_inscriptions
      .iter()
      .map(|flotsam| flotsam.inscription_id)
      .collect::<HashSet<InscriptionId>>();

    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New {
          parents: purported_parents,
          ..
        },
        ..
      } = flotsam
      {
        let mut seen = HashSet::new();
        purported_parents
          .retain(|parent| seen.insert(*parent) && potential_parents.contains(parent));
      }
    }

    // still have to normalize over inscription size
    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New { fee, .. },
        ..
      } = flotsam
      {
        *fee = (total_input_value - total_output_value) / u64::from(id_counter);
      }
    }

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

    let mut new_locations = Vec::new();
    let mut output_value = 0;
    let mut inscription_idx = 0;
    for (vout, txout) in tx.output.iter().enumerate() {
      let end = output_value + txout.value.to_sat();

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

        new_locations.push((
          new_satpoint,
          sent_to_coinbase, txout,
          inscriptions.next().unwrap(),
          txout.script_pubkey.is_op_return(),
        ));
      }

      output_value = end;
    }

    for (new_satpoint, sent_to_coinbase, tx_out, flotsam, op_return) in new_locations.into_iter() {
      let output_utxo_entry =
        &mut output_utxo_entries[usize::try_from(new_satpoint.outpoint.vout).unwrap()];


      let tx = flotsam.tx_option.clone().unwrap();
      self.update_inscription_location(
        Some(&tx),
        Some(&tx_out.script_pubkey),
        Some(&tx_out.value.to_sat()),
        sent_to_coinbase,
        input_sat_ranges,
        flotsam,
        new_satpoint,
        op_return,
        Some(output_utxo_entry),
        utxo_cache,
        index,
      )?;
    }

    if is_coinbase {
      for flotsam in inscriptions {
        let new_satpoint = SatPoint {
          outpoint: OutPoint::null(),
          offset: flotsam.offset - output_value,
        };
        let tx = flotsam.tx_option.clone().unwrap();
        self.update_inscription_location(
          Some(&tx),
          None,
          None,
          true,
          input_sat_ranges,
          flotsam,
          new_satpoint,
          false,
          None,
          utxo_cache,
          index,
        )?;
      }
      Ok(())
    } else {
      for flotsam in inscriptions {
        self.flotsam.push(Flotsam {
          offset: self.reward + flotsam.offset - output_value,
          ..flotsam
        });

        // ord indexes sent as fee transfers at the end of the block but it would make more sense if they were indexed as soon as they are sent
        // self.write_to_file(format!("cmd;{0};insert;early_transfer_sent_as_fee;{1}", self.height, flotsam.inscription_id), true)?;
        self.transfer_idx += 1;
        self.early_transfer_info.insert(flotsam.inscription_id, self.transfer_idx);
      }
      self.reward += total_input_value - output_value;
      Ok(())
    }
  }

  fn get_json_tx_limit(inscription_content_option: &Option<Vec<u8>>) -> i16 {
    if inscription_content_option.is_none() { return 0; }
    let inscription_content = inscription_content_option.as_ref().unwrap();

    let json = serde_json::from_slice::<Value>(&inscription_content);
    if json.is_err() {
      return 0;
    } else {
      // check for event type and return tx limit
      return TX_LIMITS["default"];
    }
  }

  fn is_text(inscription_content_type_option: &Option<Vec<u8>>) -> bool {
    if inscription_content_type_option.is_none() { return false; }

    let inscription_content_type = inscription_content_type_option.as_ref().unwrap();
    let inscription_content_type_str = std::str::from_utf8(&inscription_content_type).unwrap_or("");
    return inscription_content_type_str == "text/plain" || inscription_content_type_str.starts_with("text/plain;") ||
            inscription_content_type_str == "application/json" || inscription_content_type_str.starts_with("application/json;"); // NOTE: added application/json for JSON5 etc.
  }

  /*fn write_to_file(
    &mut self,
    to_write: String,
    flush: bool,
  ) -> Result {
    lazy_static! {
      static ref LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
    }
    let mut log_file = LOG_FILE.lock().unwrap();
    if log_file.as_ref().is_none() {
      let chain_folder: String = match self.chain {
        Chain::Mainnet => String::from(""),
        Chain::Testnet => String::from("testnet3/"),
        Chain::Testnet4 => String::from("testnet4/"),
        Chain::Signet => String::from("signet/"),
        Chain::Regtest => String::from("regtest/"),
      };
      *log_file = Some(File::options().append(true).open(format!("{chain_folder}log_file.txt")).unwrap());
    }
    if to_write != "" {
      if self.first_in_block {
        println!("cmd;{0};block_start", self.height,);
        writeln!(log_file.as_ref().unwrap(), "cmd;{0};block_start", self.height,)?;
      }
      self.first_in_block = false;

      writeln!(log_file.as_ref().unwrap(), "{}", to_write)?;
    }
    if flush {
      (log_file.as_ref().unwrap()).flush()?;
    }

    Ok(())
  }*/

  pub(super) fn end_block(
    &mut self,
  ) -> Result {
    /*if !self.first_in_block {
      println!("cmd;{0};block_end", self.height);
      self.write_to_file(format!("cmd;{0};block_end", self.height), true)?;
    }*/

    Ok(())
  }

  fn calculate_sat(input_sat_ranges: Option<&Vec<&[u8]>>, input_offset: u64) -> Option<Sat> {
    let input_sat_ranges = input_sat_ranges?;

    let mut offset = 0;
    for chunk in input_sat_ranges
      .iter()
      .flat_map(|slice| slice.chunks_exact(11))
    {
      let (start, end) = SatRange::load(chunk.try_into().unwrap());
      let size = end - start;
      if offset + size > input_offset {
        let n = start + input_offset - offset;
        return Some(Sat(n));
      }
      offset += size;
    }

    unreachable!()
  }

  fn update_inscription_location(
    &mut self,
    tx_option: Option<&Transaction>,
    new_script_pubkey: Option<&ScriptBuf>,
    new_output_value: Option<&u64>,
    send_to_coinbase: bool,
    input_sat_ranges: Option<&Vec<&[u8]>>,
    flotsam: Flotsam,
    new_satpoint: SatPoint,
    op_return: bool,
    mut normal_output_utxo_entry: Option<&mut UtxoEntryBuf>,
    utxo_cache: &mut HashMap<OutPoint, UtxoEntryBuf>,
    index: &Index,
  ) -> Result {
    let tx = tx_option.unwrap();
    let inscription_id = flotsam.inscription_id;
    let txcnt_of_inscr: i64 = self.db.get_cf(self.id_to_txcnt, &inscription_id.store())?
        .map(|txcnt| i64::from_be_bytes(txcnt.try_into().unwrap()))
        .unwrap_or(0) + 1;
    if txcnt_of_inscr == 1 {
      self.db.put_cf_opt(self.id_to_txcnt, &inscription_id.store(), &txcnt_of_inscr.to_be_bytes(), self.write_options)?;
    }

    let (unbound, sequence_number) = match flotsam.origin {
      Origin::Old {
        sequence_number,
        old_satpoint,
      } => {
        if let Some(ref sender) = index.event_sender {
          sender.blocking_send(Event::InscriptionTransferred {
            block_height: self.height,
            inscription_id,
            new_location: new_satpoint,
            old_location: old_satpoint,
            sequence_number,
          })?;
        }

        let entry = self.db.get_cf(self.sequence_number_to_entry, &sequence_number.to_be_bytes())?;
        let entry = entry
          .map(|entry| InscriptionEntry::load(entry))
          .unwrap();
        let is_json_or_text = entry.is_json_or_text;
        let txcnt_limit = entry.txcnt_limit;
        if is_json_or_text && txcnt_of_inscr <= txcnt_limit.into() { // only track non-cursed and first two transactions
          let transfer_idx = if self.early_transfer_info.contains_key(&inscription_id) {
            self.early_transfer_info[&inscription_id]
          } else {
            self.transfer_idx += 1;
            self.transfer_idx
          };
          let transfer_key = [
            self.height.to_be_bytes(),
            transfer_idx.to_be_bytes(),
          ].concat();
          let transfer_data = [
            flotsam.inscription_id.store(),
            old_satpoint.store().to_vec(),
            new_satpoint.store().to_vec(),
            vec![send_to_coinbase as u8],
            new_output_value.unwrap_or(&0).to_be_bytes().to_vec(),
            new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes(),
          ].concat();
          self.db.put_cf_opt(self.ord_transfers, &transfer_key, &transfer_data, self.write_options)?;

          /* self.write_to_file(format!("cmd;{0};insert;transfer;{1};{old_satpoint};{new_satpoint};{send_to_coinbase};{2};{3}",
                    self.height, flotsam.inscription_id,
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes()),
                    new_output_value.unwrap_or(&0)), false)?; */

          if txcnt_of_inscr != 1 {
            self.db.put_cf_opt(self.id_to_txcnt, &inscription_id.store(), &txcnt_of_inscr.to_be_bytes(), self.write_options)?;
          }
        }

        (false, sequence_number)
      }
      Origin::New {
        cursed,
        cursed_for_brc20,
        fee: _,
        parents,
        reinscription,
        unbound,
        vindicated,
      } => {
        let inscription_number = if cursed {
          let number: i32 = self.cursed_inscription_count.try_into().unwrap();
          self.cursed_inscription_count += 1;
          -(number + 1)
        } else {
          let number: i32 = self.blessed_inscription_count.try_into().unwrap();
          self.blessed_inscription_count += 1;
          number
        };

        let sequence_number = self.next_sequence_number;
        self.next_sequence_number += 1;

        self
          .db
          .put_cf_opt(self.inscription_number_to_sequence_number, inscription_number.to_be_bytes(), sequence_number.to_be_bytes(), self.write_options)?;

        let inscription = ParsedEnvelope::from_transaction(&tx)
            .get(flotsam.inscription_id.index as usize)
            .unwrap()
            .payload.clone();
        let inscription_content = inscription.body;
        let inscription_content_type = inscription.content_type;
        let inscription_metaprotocol = inscription.metaprotocol;
        let json_txcnt_limit = Self::get_json_tx_limit(&inscription_content);
        let is_json = json_txcnt_limit > 0;
        let is_text = Self::is_text(&inscription_content_type);
        let is_json_or_text = is_json || is_text;

        let txcnt_limit = if !unbound && is_json_or_text {
          let inscription_id_key = flotsam.inscription_id.store();
          let inscription_info_data = [
            inscription_number.to_be_bytes().to_vec(),
            vec![cursed_for_brc20 as u8],
            parents.get(0).map(|p| p.store()).unwrap_or(InscriptionId::from_str("0000000000000000000000000000000000000000000000000000000000000000i0")?.store()),
            vec![is_json as u8],
            inscription_content.as_ref().map(|content| content.len() as u32).unwrap_or(0).to_be_bytes().to_vec(),
            inscription_content.unwrap_or(Vec::new()),
            inscription_content_type.as_ref().map(|content_type| content_type.len() as u32).unwrap_or(0).to_be_bytes().to_vec(),
            inscription_content_type.unwrap_or(Vec::new()),
            inscription_metaprotocol.as_ref().map(|metaprotocol| metaprotocol.len() as u32).unwrap_or(0).to_be_bytes().to_vec(),
            inscription_metaprotocol.unwrap_or(Vec::new()),
          ].concat();
          self.db.put_cf_opt(self.ord_inscription_info, &inscription_id_key, &inscription_info_data, self.write_options)?;


          //self.write_to_file(format!("cmd;{0};insert;number_to_id;{1};{2};{3};{4}", self.height, inscription_number, flotsam.inscription_id, if cursed_for_brc20 {"1"} else {"0"}, parents.get(0).map(|p| p.to_string()).unwrap_or(String::from(""))), false)?;
          // write content as minified json
          if is_json {
            /* let inscription_content_json = serde_json::from_slice::<Value>(&(inscription_content.unwrap())).unwrap();
            let inscription_content_json_str = serde_json::to_string(&inscription_content_json).unwrap();
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            let inscription_metaprotocol_str = hex::encode(inscription_metaprotocol.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4};{5}",
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_metaprotocol_str, inscription_content_json_str), false)?; */

            json_txcnt_limit
          } else {
            /* let inscription_content_hex_str = hex::encode(inscription_content.unwrap_or(Vec::new()));
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            let inscription_metaprotocol_str = hex::encode(inscription_metaprotocol.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4};{5}",
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_metaprotocol_str, inscription_content_hex_str), false)?; */

            TX_LIMITS["default"]
          }
        } else {
          0
        };

        let sat = if unbound {
          None
        } else {
          Self::calculate_sat(input_sat_ranges, flotsam.offset)
        };

        let mut charms = 0;

        if cursed {
          Charm::Cursed.set(&mut charms);
        }

        if reinscription {
          Charm::Reinscription.set(&mut charms);
        }

        if let Some(sat) = sat {
          charms |= sat.charms();
        }

        if op_return {
          Charm::Burned.set(&mut charms);
        }

        if new_satpoint.outpoint == OutPoint::null() {
          Charm::Lost.set(&mut charms);
        }

        if unbound {
          Charm::Unbound.set(&mut charms);
        }

        if vindicated {
          Charm::Vindicated.set(&mut charms);
        }

        if let Some(ref sender) = index.event_sender {
          sender.blocking_send(Event::InscriptionCreated {
            block_height: self.height,
            charms,
            inscription_id,
            location: (!unbound).then_some(new_satpoint),
            parent_inscription_ids: parents,
            sequence_number,
          })?;
        }

        self.db.put_cf_opt(
          self.sequence_number_to_entry,
          sequence_number.to_be_bytes(),
          &InscriptionEntry {
            charms,
            id: inscription_id,
            inscription_number,
            sequence_number,
            is_json_or_text,
            txcnt_limit,
          }
          .store(),
          self.write_options,
        )?;

        self
          .db
          .put_cf_opt(self.id_to_sequence_number, &inscription_id.store(), sequence_number.to_be_bytes(), self.write_options)?;

        if !unbound && is_json_or_text {
          let transfer_idx = if self.early_transfer_info.contains_key(&inscription_id) {
            self.early_transfer_info[&inscription_id]
          } else {
            self.transfer_idx += 1;
            self.transfer_idx
          };
          let transfer_key = [
            self.height.to_be_bytes(),
            transfer_idx.to_be_bytes(),
          ].concat();
          let transfer_data = [
            flotsam.inscription_id.store(),
            SatPoint {
              outpoint: OutPoint::null(),
              offset: 0,
            }.store().to_vec(),
            new_satpoint.store().to_vec(),
            vec![send_to_coinbase as u8],
            new_output_value.unwrap_or(&0).to_be_bytes().to_vec(),
            new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes(),
          ].concat();
          self.db.put_cf_opt(self.ord_transfers, &transfer_key, &transfer_data, self.write_options)?;

          /* self.write_to_file(format!("cmd;{0};insert;transfer;{1};;{new_satpoint};{send_to_coinbase};{2};{3}",
                    self.height, flotsam.inscription_id,
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes()),
                    new_output_value.unwrap_or(&0)), false)?; */
        }

        (unbound, sequence_number)
      }
    };

    let satpoint = if unbound {
      let new_unbound_satpoint = SatPoint {
        outpoint: unbound_outpoint(),
        offset: 0,
      };
      normal_output_utxo_entry = None;
      new_unbound_satpoint
    } else {
      new_satpoint
    };

    // The special outpoints, i.e., the null outpoint and the unbound outpoint,
    // don't follow the normal rules. Unlike real outputs they get written to
    // more than once. So we create a new UTXO entry here and commit() will
    // merge it with any existing entry.
    let output_utxo_entry = normal_output_utxo_entry.unwrap_or_else(|| {
      assert!(Index::is_special_outpoint(satpoint.outpoint));
      utxo_cache
        .entry(satpoint.outpoint)
        .or_insert(UtxoEntryBuf::empty())
    });

    output_utxo_entry.push_inscription(sequence_number, satpoint.offset);

    // self.write_to_file("".to_string(), true)?;

    Ok(())
  }
}
