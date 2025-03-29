use super::*;
use {
  bitcoin::{
    blockdata::{opcodes},
    secp256k1::{XOnlyPublicKey},
    script::{ScriptBuf},
    key::{TweakedPublicKey},
  },
};
use std::fs::File;
use serde_json::Value;
use hex;

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
    hidden: bool,
    parent: Option<InscriptionId>,
    pointer: Option<u64>,
    reinscription: bool,
    unbound: bool,
    tapscript_pk: [u8; 35],
  },
  Old {
    old_satpoint: SatPoint,
  },
}

pub(super) struct InscriptionUpdater<'a, 'db, 'tx> {
  pub(super) blessed_inscription_count: u64,
  pub(super) chain: Chain,
  pub(super) cursed_inscription_count: u64,
  pub(super) flotsam: Vec<Flotsam<'a>>,
  pub(super) height: u32,
  pub(super) home_inscription_count: u64,
  pub(super) home_inscriptions: &'a mut Table<'db, 'tx, u32, InscriptionIdValue>,
  pub(super) id_to_sequence_number: &'a mut Table<'db, 'tx, InscriptionIdValue, u32>,
  pub(super) index_transactions: bool,
  pub(super) inscription_number_to_sequence_number: &'a mut Table<'db, 'tx, i32, u32>,
  pub(super) id_to_txcnt: &'a mut Table<'db, 'tx, InscriptionIdValue, i64>,
  pub(super) lost_sats: u64,
  pub(super) next_sequence_number: u32,
  pub(super) outpoint_to_value: &'a mut Table<'db, 'tx, &'static OutPointValue, u64>,
  pub(super) reward: u64,
  pub(super) transaction_buffer: Vec<u8>,
  pub(super) transaction_id_to_transaction:
    &'a mut Table<'db, 'tx, &'static TxidValue, &'static [u8]>,
  pub(super) sat_to_sequence_number: &'a mut MultimapTable<'db, 'tx, u64, u32>,
  pub(super) satpoint_to_sequence_number:
    &'a mut MultimapTable<'db, 'tx, &'static SatPointValue, u32>,
  pub(super) sequence_number_to_children: &'a mut MultimapTable<'db, 'tx, u32, u32>,
  pub(super) sequence_number_to_entry: &'a mut Table<'db, 'tx, u32, InscriptionEntryValue>,
  pub(super) sequence_number_to_satpoint: &'a mut Table<'db, 'tx, u32, &'static SatPointValue>,
  pub(super) timestamp: u32,
  pub(super) unbound_inscriptions: u64,
  pub(super) value_cache: &'a mut HashMap<OutPoint, u64>,
  pub(super) value_receiver: &'a mut Receiver<u64>,
  pub(super) first_in_block: bool,
}

impl<'a, 'db, 'tx> InscriptionUpdater<'a, 'db, 'tx> {
  pub(super) fn index_envelopes(
    &mut self,
    tx: &'a Transaction,
    txid: Txid,
    input_sat_ranges: Option<&VecDeque<(u64, u64)>>,
  ) -> Result {
    let mut floating_inscriptions = Vec::new();
    let mut id_counter = 0;
    let mut inscribed_offsets = BTreeMap::new();
    let mut total_input_value = 0;
    let total_output_value = tx.output.iter().map(|txout| txout.value).sum::<u64>();

    let envelopes = ParsedEnvelope::from_transaction(tx);
    let inscriptions = !envelopes.is_empty();
    let mut envelopes = envelopes.into_iter().peekable();

    for (input_index, tx_in) in tx.input.iter().enumerate() {
      // skip subsidy since no inscriptions possible
      if tx_in.previous_output.is_null() {
        total_input_value += Height(self.height).subsidy();
        continue;
      }

      // find existing inscriptions on input (transfers of inscriptions)
      for (old_satpoint, inscription_id) in Index::inscriptions_on_output(
        self.satpoint_to_sequence_number,
        self.sequence_number_to_entry,
        tx_in.previous_output,
      )? {
        let offset = total_input_value + old_satpoint.offset;
        floating_inscriptions.push(Flotsam {
          offset,
          inscription_id,
          origin: Origin::Old { old_satpoint },
          tx_option: Some(&tx),
        });

        inscribed_offsets
          .entry(offset)
          .or_insert((inscription_id, 0))
          .1 += 1;
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
      while let Some(inscription) = envelopes.peek() {
        if inscription.input != u32::try_from(input_index).unwrap() {
          break;
        }

        let mut tapscript_pk = [0u8; 35];
        if let Some(tapscript) = tx_in.witness.tapscript() {
          if tapscript.len() >= 35 {
            let script_bytes = tapscript.as_bytes();
            if script_bytes[0] == opcodes::all::OP_PUSHBYTES_32.to_u8()
              && script_bytes[33] == opcodes::all::OP_CHECKSIGVERIFY.to_u8()
              && script_bytes[34] >= opcodes::all::OP_PUSHNUM_1.to_u8()
              && script_bytes[34] <= opcodes::all::OP_PUSHNUM_6.to_u8() {
                tapscript_pk.copy_from_slice(&script_bytes[..35]);
              }
          }
        }

        let inscription_id = InscriptionId {
          txid,
          index: id_counter,
        };

        let curse = if self.height >= self.chain.jubilee_height() {
          None
        } else if inscription.payload.unrecognized_even_field {
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
              self.id_to_sequence_number.get(id.store())?.unwrap().value();

            let initial_inscription_is_cursed = InscriptionEntry::load(
              self
                .sequence_number_to_entry
                .get(initial_inscription_sequence_number)?
                .unwrap()
                .value(),
            )
            .inscription_number
              < 0;

            if initial_inscription_is_cursed {
              None
            } else {
              Some(Curse::Reinscription)
            }
          }
        } else {
          None
        };

        let cursed_for_brc20 = if inscription.payload.unrecognized_even_field {
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
              self.id_to_sequence_number.get(id.store())?.unwrap().value();

            let initial_inscription_is_cursed = InscriptionEntry::load(
              self
                .sequence_number_to_entry
                .get(initial_inscription_sequence_number)?
                .unwrap()
                .value(),
            )
            .is_cursed_for_brc20; // NOTE: CHANGED TO BE SAME AS 0.9 RULES

            if initial_inscription_is_cursed {
              None
            } else {
              Some(Curse::Reinscription)
            }
          }
        } else {
          None
        };

        let unbound = current_input_value == 0
          || curse == Some(Curse::UnrecognizedEvenField)
          || inscription.payload.unrecognized_even_field;

        let offset = inscription
          .payload
          .pointer()
          .filter(|&pointer| pointer < total_output_value)
          .unwrap_or(offset);

        floating_inscriptions.push(Flotsam {
          inscription_id,
          offset,
          origin: Origin::New {
            reinscription: inscribed_offsets.get(&offset).is_some(),
            cursed: curse.is_some(),
            cursed_for_brc20: cursed_for_brc20.is_some(),
            fee: 0,
            hidden: inscription.payload.hidden(),
            parent: inscription.payload.parent(),
            pointer: inscription.payload.pointer(),
            unbound,
            tapscript_pk,
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

    if self.index_transactions && inscriptions {
      tx.consensus_encode(&mut self.transaction_buffer)
        .expect("in-memory writers don't error");

      self
        .transaction_id_to_transaction
        .insert(&txid.store(), self.transaction_buffer.as_slice())?;

      self.transaction_buffer.clear();
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
    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New { ref mut fee, .. },
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

    let mut range_to_vout = BTreeMap::new();
    let mut new_locations = Vec::new();
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

        new_locations.push((new_satpoint, sent_to_coinbase, tx_out, inscriptions.next().unwrap()));
      }

      range_to_vout.insert((output_value, end), vout.try_into().unwrap());

      output_value = end;

      self.value_cache.insert(
        OutPoint {
          vout: vout.try_into().unwrap(),
          txid,
        },
        tx_out.value,
      );
    }

    for (new_satpoint, sent_to_coinbase, tx_out, mut flotsam) in new_locations.into_iter() {
      let new_satpoint = match flotsam.origin {
        Origin::New {
          pointer: Some(pointer),
          ..
        } if pointer < output_value => {
          match range_to_vout.iter().find_map(|((start, end), vout)| {
            (pointer >= *start && pointer < *end).then(|| (vout, pointer - start))
          }) {
            Some((vout, offset)) => {
              flotsam.offset = pointer;
              SatPoint {
                outpoint: OutPoint { txid, vout: *vout },
                offset,
              }
            }
            _ => new_satpoint,
          }
        }
        _ => new_satpoint,
      };

      let tx = flotsam.tx_option.clone().unwrap();
      self.update_inscription_location(
        Some(&tx),
        Some(&tx_out.script_pubkey),
        Some(&tx_out.value),
        input_sat_ranges,
        flotsam,
        new_satpoint,
        sent_to_coinbase,
      )?;
    }

    if is_coinbase {
      for flotsam in inscriptions {
        let new_satpoint = SatPoint {
          outpoint: OutPoint::null(),
          offset: self.lost_sats + flotsam.offset - output_value,
        };
        let tx = flotsam.tx_option.clone().unwrap();
        self.update_inscription_location(Some(&tx), None, None, input_sat_ranges, flotsam, new_satpoint, true)?;
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
    let input_sat_ranges = input_sat_ranges?;

    let mut offset = 0;
    for (start, end) in input_sat_ranges {
      let size = end - start;
      if offset + size > input_offset {
        let n = start + input_offset - offset;
        return Some(Sat(n));
      }
      offset += size;
    }

    unreachable!()
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

  fn write_to_file(
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
    new_output_value: Option<&u64>,
    input_sat_ranges: Option<&VecDeque<(u64, u64)>>,
    flotsam: Flotsam,
    new_satpoint: SatPoint,
    send_to_coinbase: bool,
  ) -> Result {
    let tx = tx_option.unwrap();
    let inscription_id = flotsam.inscription_id;
    let txcnt_of_inscr: i64 = self.id_to_txcnt.get(&inscription_id.store())?
        .map(|txcnt| txcnt.value())
        .unwrap_or(0) + 1;
    self.id_to_txcnt.insert(&inscription_id.store(), &txcnt_of_inscr)?;

    let (unbound, sequence_number) = match flotsam.origin {
      Origin::Old { old_satpoint } => {
        self
          .satpoint_to_sequence_number
          .remove_all(&old_satpoint.store())?;

        let sequence_number =  self
            .id_to_sequence_number
            .get(&inscription_id.store())?
            .unwrap()
            .value();
        // get is_json_or_text from id_to_entry
        let entry = self.sequence_number_to_entry.get(&sequence_number)?;
        let entry = entry
          .map(|entry| InscriptionEntry::load(entry.value()))
          .unwrap();
        let is_json_or_text = entry.is_json_or_text;
        let txcnt_limit = entry.txcnt_limit;
        if is_json_or_text && txcnt_of_inscr <= txcnt_limit.into() { // only track non-cursed and first two transactions
          self.write_to_file(format!("cmd;{0};insert;transfer;{1};{old_satpoint};{new_satpoint};{send_to_coinbase};{2};{3}", 
                    self.height, flotsam.inscription_id, 
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes()), 
                    new_output_value.unwrap_or(&0)), false)?;
        }

        (
          false,
          sequence_number,
        )
      }
      Origin::New {
        cursed,
        cursed_for_brc20,
        fee,
        hidden,
        parent,
        pointer: _,
        reinscription,
        unbound,
        tapscript_pk,
      } => {
        let inscription_number = if cursed {
          let number: i32 = self.cursed_inscription_count.try_into().unwrap();
          self.cursed_inscription_count += 1;

          // because cursed numbers start at -1
          -(number + 1)
        } else {
          let number: i32 = self.blessed_inscription_count.try_into().unwrap();
          self.blessed_inscription_count += 1;

          number
        };

        let sequence_number = self.next_sequence_number;
        self.next_sequence_number += 1;

        self
          .inscription_number_to_sequence_number
          .insert(inscription_number, sequence_number)?;

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
          let signer_pkscript = get_pk_script_by_pubkey_and_type(&tapscript_pk[1..33], tapscript_pk[34]).to_hex_string();
          self.write_to_file(format!("cmd;{0};insert;number_to_id;{1};{2};{3};{4};{5}", self.height, inscription_number, flotsam.inscription_id, if cursed_for_brc20 {"1"} else {"0"}, parent.map(|p| p.to_string()).unwrap_or(String::from("")), signer_pkscript), false)?;
          // write content as minified json
          if is_json {
            let inscription_content_json = serde_json::from_slice::<Value>(&(inscription_content.unwrap())).unwrap();
            let inscription_content_json_str = serde_json::to_string(&inscription_content_json).unwrap();
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            let inscription_metaprotocol_str = hex::encode(inscription_metaprotocol.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4};{5}", 
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_metaprotocol_str, inscription_content_json_str), false)?;
            
            json_txcnt_limit
          } else {
            let inscription_content_hex_str = hex::encode(inscription_content.unwrap_or(Vec::new()));
            let inscription_content_type_str = hex::encode(inscription_content_type.unwrap_or(Vec::new()));
            let inscription_metaprotocol_str = hex::encode(inscription_metaprotocol.unwrap_or(Vec::new()));
            self.write_to_file(format!("cmd;{0};insert;content;{1};{2};{3};{4};{5}", 
                                    self.height, flotsam.inscription_id, is_json, inscription_content_type_str, inscription_metaprotocol_str, inscription_content_hex_str), false)?;
            
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
          if sat.nineball() {
            Charm::Nineball.set(&mut charms);
          }

          if sat.coin() {
            Charm::Coin.set(&mut charms);
          }

          match sat.rarity() {
            Rarity::Common | Rarity::Mythic => {}
            Rarity::Uncommon => Charm::Uncommon.set(&mut charms),
            Rarity::Rare => Charm::Rare.set(&mut charms),
            Rarity::Epic => Charm::Epic.set(&mut charms),
            Rarity::Legendary => Charm::Legendary.set(&mut charms),
          }
        }

        if new_satpoint.outpoint == OutPoint::null() {
          Charm::Lost.set(&mut charms);
        }

        if unbound {
          Charm::Unbound.set(&mut charms);
        }

        if let Some(Sat(n)) = sat {
          self.sat_to_sequence_number.insert(&n, &sequence_number)?;
        }

        let parent = match parent {
          Some(parent_id) => {
            let parent_sequence_number = self
              .id_to_sequence_number
              .get(&parent_id.store())?
              .unwrap()
              .value();
            self
              .sequence_number_to_children
              .insert(parent_sequence_number, sequence_number)?;

            Some(parent_sequence_number)
          }
          None => None,
        };

        self.sequence_number_to_entry.insert(
          sequence_number,
          &InscriptionEntry {
            charms,
            fee,
            height: self.height,
            id: inscription_id,
            inscription_number,
            parent,
            sat,
            sequence_number,
            timestamp: self.timestamp,
            is_json_or_text,
            is_cursed_for_brc20: cursed_for_brc20,
            txcnt_limit,
          }
          .store(),
        )?;

        self
          .id_to_sequence_number
          .insert(&inscription_id.store(), sequence_number)?;

        if !hidden {
          self
            .home_inscriptions
            .insert(&sequence_number, inscription_id.store())?;

          if self.home_inscription_count == 100 {
            self.home_inscriptions.pop_first()?;
          } else {
            self.home_inscription_count += 1;
          }
        }

        if !unbound && is_json_or_text {
          self.write_to_file(format!("cmd;{0};insert;transfer;{1};;{new_satpoint};{send_to_coinbase};{2};{3}", 
                    self.height, flotsam.inscription_id, 
                    hex::encode(new_script_pubkey.unwrap_or(&ScriptBuf::new()).clone().into_bytes()), 
                    new_output_value.unwrap_or(&0)), false)?;
        }

        (unbound, sequence_number)
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

    self
      .satpoint_to_sequence_number
      .insert(&satpoint, sequence_number)?;
    self
      .sequence_number_to_satpoint
      .insert(sequence_number, &satpoint)?;

    self.write_to_file("".to_string(), true)?;

    Ok(())
  }
}

/// Get a script pubkey based on the provided pubkey and address type
pub fn get_pk_script_by_pubkey_and_type(x_only_pubkey_bytes: &[u8], address_type: u8) -> ScriptBuf {
  const BRC20_PUBKEY_ADDRESS_P2TR_SCRIPT: u8 = 0x51;
  const BRC20_PUBKEY_ADDRESS_P2PKH_EVEN: u8 = 0x52;
  const BRC20_PUBKEY_ADDRESS_P2PKH_ODD: u8 = 0x53;
  const BRC20_PUBKEY_ADDRESS_P2WPKH: u8 = 0x54;
  const BRC20_PUBKEY_ADDRESS_P2TR_KEY: u8 = 0x55;
  const BRC20_PUBKEY_ADDRESS_P2SH_P2WPKH: u8 = 0x56;

  let x_only_pubkey = XOnlyPublicKey::from_slice(x_only_pubkey_bytes).unwrap();
  match address_type {
    BRC20_PUBKEY_ADDRESS_P2TR_SCRIPT => {
      let secp = bitcoin::secp256k1::Secp256k1::verification_only();
      ScriptBuf::new_v1_p2tr(&secp, x_only_pubkey, None)
    },
    BRC20_PUBKEY_ADDRESS_P2PKH_EVEN | BRC20_PUBKEY_ADDRESS_P2PKH_ODD => {
      let parity = if address_type == BRC20_PUBKEY_ADDRESS_P2PKH_EVEN {
        bitcoin::secp256k1::Parity::Even
      } else {
        bitcoin::secp256k1::Parity::Odd
      };
      let pubkey = bitcoin::PublicKey::new(x_only_pubkey.public_key(parity));
      ScriptBuf::new_p2pkh(&pubkey.pubkey_hash())
    },
    BRC20_PUBKEY_ADDRESS_P2WPKH | BRC20_PUBKEY_ADDRESS_P2SH_P2WPKH => {
      let pubkey = bitcoin::PublicKey::new(x_only_pubkey.public_key(bitcoin::secp256k1::Parity::Even));
      let wpkh_script = ScriptBuf::new_v0_p2wpkh(&pubkey.wpubkey_hash().unwrap());
      if address_type == BRC20_PUBKEY_ADDRESS_P2WPKH {
        wpkh_script
      } else {
        ScriptBuf::new_p2sh(&wpkh_script.script_hash())
      }
    },
    BRC20_PUBKEY_ADDRESS_P2TR_KEY => {
      ScriptBuf::new_v1_p2tr_tweaked(TweakedPublicKey::dangerous_assume_tweaked(x_only_pubkey))
    },
    _ => ScriptBuf::new(),
  }
}
