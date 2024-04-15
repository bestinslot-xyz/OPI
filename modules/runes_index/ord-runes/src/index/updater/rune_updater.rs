use super::*;

pub(super) struct RuneUpdater<'a, 'tx, 'client> {
  pub(super) block_time: u32,
  pub(super) burned: HashMap<RuneId, Lot>,
  pub(super) minted: HashMap<RuneId, u128>,
  pub(super) client: &'client Client,
  pub(super) height: u32,
  pub(super) id_to_entry: &'a mut Table<'tx, RuneIdValue, RuneEntryValue>,
  pub(super) inscription_id_to_sequence_number: &'a Table<'tx, InscriptionIdValue, u32>,
  pub(super) minimum: Rune,
  pub(super) outpoint_to_balances: &'a mut Table<'tx, &'static OutPointValue, &'static [u8]>,
  pub(super) rune_to_id: &'a mut Table<'tx, u128, RuneIdValue>,
  pub(super) runes: u64,
  pub(super) sequence_number_to_rune_id: &'a mut Table<'tx, u32, RuneIdValue>,
  pub(super) statistic_to_count: &'a mut Table<'tx, u64, u64>,
  pub(super) transaction_id_to_rune: &'a mut Table<'tx, &'static TxidValue, u128>,
  pub(super) first_in_block: bool,
  pub(super) chain: Chain,
}

use std::fs::File;

impl<'a, 'tx, 'client> RuneUpdater<'a, 'tx, 'client> {
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

  pub(super) fn index_runes(&mut self, tx_index: u32, tx: &Transaction, txid: Txid) -> Result<()> {
    let artifact = Runestone::decipher(tx);

    let (mut unallocated, tx_inputs) = self.unallocated(tx)?;
    let mut new_rune_allocations: HashMap<RuneId, u128> = HashMap::new();
    let mut mints: HashMap<RuneId, u128> = HashMap::new();

    let mut allocated: Vec<HashMap<RuneId, Lot>> = vec![HashMap::new(); tx.output.len()];

    if let Some(artifact) = &artifact {
      if let Some(id) = artifact.mint() {
        if let Some(amount) = self.mint(id)? {
          *unallocated.entry(id).or_default() += amount;
          *mints.entry(id).or_default() += amount.n();
        }
      }

      let etched = self.etched(tx_index, tx, artifact)?;

      if let Artifact::Runestone(runestone) = artifact {
        if let Some((id, ..)) = etched {
          *unallocated.entry(id).or_default() +=
            runestone.etching.unwrap().premine.unwrap_or_default();
          *new_rune_allocations.entry(id).or_default() += 
            runestone.etching.unwrap().premine.unwrap_or_default();
        }

        for Edict { id, amount, output } in runestone.edicts.iter().copied() {
          let amount = Lot(amount);

          // edicts with output values greater than the number of outputs
          // should never be produced by the edict parser
          let output = usize::try_from(output).unwrap();
          assert!(output <= tx.output.len());

          let id = if id == RuneId::default() {
            let Some((id, ..)) = etched else {
              continue;
            };

            id
          } else {
            id
          };

          let Some(balance) = unallocated.get_mut(&id) else {
            continue;
          };

          let mut allocate = |balance: &mut Lot, amount: Lot, output: usize| {
            if amount > 0 {
              *balance -= amount;
              *allocated[output].entry(id).or_default() += amount;
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
                allocate(
                  balance,
                  if i < remainder { amount + 1 } else { amount },
                  *output,
                );
              }
            } else {
              // if amount is non-zero, distribute amount to eligible outputs
              for output in destinations {
                allocate(balance, amount.min(*balance), output);
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
          }
        }
      }

      if let Some((id, rune)) = etched {
        self.create_rune_entry(txid, artifact, id, rune)?;
      }
    }

    let mut burned: HashMap<RuneId, Lot> = HashMap::new();

    if let Some(Artifact::Cenotaph(_)) = artifact {
      for (id, balance) in unallocated {
        *burned.entry(id).or_default() += balance;
      }
    } else {
      let pointer = artifact
        .map(|artifact| match artifact {
          Artifact::Runestone(runestone) => runestone.pointer,
          Artifact::Cenotaph(_) => unreachable!(),
        })
        .unwrap_or_default();

      // assign all un-allocated runes to the default output, or the first non
      // OP_RETURN output if there is no default, or if the default output is
      // too large
      if let Some(vout) = pointer
        .map(|pointer| pointer.into_usize())
        .inspect(|&pointer| assert!(pointer < allocated.len()))
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

    // use tx_inputs, new_rune_allocations, mints, burned, and allocated to get event string
    // cmd;<height>;tx_events_input;<txid>;<outpoint>;<id>;<amount>
    // cmd;<height>;tx_events_new_rune_allocation;<txid>;<id>;<amount>
    // cmd;<height>;tx_events_mint;<txid>;<id>;<amount>
    // cmd;<height>;tx_events_output;<txid>;<outpoint>;<id>;<amount>;<scriptpubkey>
    // cmd;<height>;tx_events_burn;<txid>;<id>;<amount>

    for (outpoint, balances) in tx_inputs {
      for (rune_id, amount) in balances {
        self.write_to_file(format!("cmd;{0};tx_events_input;{1};{2};{3};{4}", self.height, txid, outpoint, rune_id, amount), false)?;
      }
    }
    for (rune_id, amount) in new_rune_allocations {
      self.write_to_file(format!("cmd;{0};tx_events_new_rune_allocation;{1};{2};{3}", self.height, txid, rune_id, amount), false)?;
    }
    for (rune_id, amount) in mints {
      self.write_to_file(format!("cmd;{0};tx_events_mint;{1};{2};{3}", self.height, txid, rune_id, amount), false)?;
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
          *burned.entry(*id).or_default() += *balance;
        }
        continue;
      }

      buffer.clear();

      let mut balances = balances.into_iter().collect::<Vec<(RuneId, Lot)>>();

      // Sort balances by id so tests can assert balances in a fixed order
      balances.sort();

      let outpoint = OutPoint {
        txid,
        vout: vout.try_into().unwrap(),
      };
      let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());

      let mut balances_str = String::new();
      for (id, balance) in balances {
        Index::encode_rune_balance(id, balance.n(), &mut buffer);
        
        balances_str += &format!("{0}-{1},", id, balance.n());
        let scriptpubkeyhex = hex::encode(tx.output[vout].script_pubkey.clone().into_bytes());

        self.write_to_file(format!("cmd;{0};tx_events_output;{1};{2};{3};{4};{5}", self.height, txid, outpoint, id, balance.n(), scriptpubkeyhex), false)?;
      }

      self.outpoint_to_balances.insert(
        &outpoint.store(),
        buffer.as_slice(),
      )?;

      self.write_to_file(format!("cmd;{0};outpoint_to_balances_insert;{1};{2};{3}", self.height, scriptpubkeyhex, outpoint, balances_str), false)?;
    }

    // increment entries with burned runes
    for (id, amount) in burned {
      *self.burned.entry(id).or_default() += amount;
      self.write_to_file(format!("cmd;{0};tx_events_burn;{1};{2};{3}", self.height, txid, id, amount.n()), false)?;
    }

    Ok(())
  }

  pub(super) fn update(&mut self) -> Result {
    for (rune_id, burned) in &self.burned {
      let mut entry = RuneEntry::load(self.id_to_entry.get(&rune_id.store())?.unwrap().value());
      entry.burned = entry.burned.checked_add(burned.n()).unwrap();
      self.id_to_entry.insert(&rune_id.store(), entry.store())?;
    }
    
    let burned_clone = self.burned.clone();
    let minted_clone = self.minted.clone();
    for (rune_id, _burned) in &burned_clone {
      let entry = RuneEntry::load(self.id_to_entry.get(&rune_id.store())?.unwrap().value());
      self.write_to_file(format!("cmd;{0};id_to_entry_update;{1};{2};{3}", self.height, rune_id, entry.burned, entry.mints), false)?;
    }

    for (rune_id, _amount) in &minted_clone {
      let burned_amount = burned_clone.get(&rune_id);
      if burned_amount.is_none() {
        let entry = RuneEntry::load(self.id_to_entry.get(&rune_id.store())?.unwrap().value());
        self.write_to_file(format!("cmd;{0};id_to_entry_update;{1};{2};{3}", self.height, rune_id, entry.burned, entry.mints), false)?;
      }
    }
    
    if !self.first_in_block {
      println!("cmd;{0};block_end", self.height);
      self.write_to_file(format!("cmd;{0};block_end", self.height), true)?;
    }

    Ok(())
  }

  fn create_rune_entry(
    &mut self,
    txid: Txid,
    artifact: &Artifact,
    id: RuneId,
    rune: Rune,
  ) -> Result {
    self.rune_to_id.insert(rune.store(), id.store())?;
    self
      .transaction_id_to_rune
      .insert(&txid.store(), rune.store())?;

    let number = self.runes;
    self.runes += 1;

    self
      .statistic_to_count
      .insert(&Statistic::Runes.into(), self.runes)?;

    let entry = match artifact {
      Artifact::Cenotaph(_) => RuneEntry {
        block: id.block,
        burned: 0,
        divisibility: 0,
        etching: txid,
        terms: None,
        mints: 0,
        number,
        premine: 0,
        spaced_rune: SpacedRune { rune, spacers: 0 },
        symbol: None,
        timestamp: self.block_time.into(),
        turbo: false,
      },
      Artifact::Runestone(Runestone { etching, .. }) => {
        let Etching {
          divisibility,
          terms,
          premine,
          spacers,
          symbol,
          turbo,
          ..
        } = etching.unwrap();

        RuneEntry {
          block: id.block,
          burned: 0,
          divisibility: divisibility.unwrap_or_default(),
          etching: txid,
          terms,
          mints: 0,
          number,
          premine: premine.unwrap_or_default(),
          spaced_rune: SpacedRune {
            rune,
            spacers: spacers.unwrap_or_default(),
          },
          symbol,
          timestamp: self.block_time.into(),
          turbo,
        }
      }
    };

    self.id_to_entry.insert(id.store(), entry.store())?;
    let mut buff_for_symbol = [0; 4];
    if entry.symbol.is_some() {
      entry.symbol.unwrap().encode_utf8(&mut buff_for_symbol);
    }
    let mut terms_str = String::new();
    if let Some(terms) = entry.terms {
      let amount = terms.amount.map_or("null".to_string(), |a| a.to_string());
      let cap = terms.cap.map_or("null".to_string(), |c| c.to_string());
      let height_l = terms.height.0.map_or("null".to_string(), |h| h.to_string());
      let height_h = terms.height.1.map_or("null".to_string(), |h| h.to_string());
      let offset_l = terms.offset.0.map_or("null".to_string(), |o| o.to_string());
      let offset_h = terms.offset.1.map_or("null".to_string(), |o| o.to_string());
      terms_str = format!("{0}-{1}-{2}-{3}-{4}-{5}", amount, cap, height_l, height_h, offset_l, offset_h);
    }
    let turbo_str = if entry.turbo { "true" } else { "false" };
    self.write_to_file(format!("cmd;{0};id_to_entry_insert;{1};{2};{3};{4};{5};{6};{7};{8};{9};{10};{11};{12};{13};{14}", 
            self.height, id, entry.block, entry.burned, entry.divisibility, 
            entry.etching, terms_str, entry.mints, entry.number, entry.premine, entry.spaced_rune.rune, entry.spaced_rune.spacers, 
            entry.symbol.map_or(String::from("null"), |_| hex::encode(buff_for_symbol)), entry.timestamp, turbo_str), false)?;


    let inscription_id = InscriptionId { txid, index: 0 };

    if let Some(sequence_number) = self
      .inscription_id_to_sequence_number
      .get(&inscription_id.store())?
    {
      self
        .sequence_number_to_rune_id
        .insert(sequence_number.value(), id.store())?;
    }

    Ok(())
  }

  fn etched(
    &mut self,
    tx_index: u32,
    tx: &Transaction,
    artifact: &Artifact,
  ) -> Result<Option<(RuneId, Rune)>> {
    let rune = match artifact {
      Artifact::Runestone(runestone) => match runestone.etching {
        Some(etching) => etching.rune,
        None => return Ok(None),
      },
      Artifact::Cenotaph(cenotaph) => match cenotaph.etching {
        Some(rune) => Some(rune),
        None => return Ok(None),
      },
    };

    let rune = if let Some(rune) = rune {
      if rune < self.minimum
        || rune.is_reserved()
        || self.rune_to_id.get(rune.0)?.is_some()
        || !self.tx_commits_to_rune(tx, rune)?
      {
        return Ok(None);
      }
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

      Rune::reserved(self.height.into(), tx_index)
    };

    Ok(Some((
      RuneId {
        block: self.height.into(),
        tx: tx_index,
      },
      rune,
    )))
  }

  fn mint(&mut self, id: RuneId) -> Result<Option<Lot>> {
    let Some(entry) = self.id_to_entry.get(&id.store())? else {
      return Ok(None);
    };

    let mut rune_entry = RuneEntry::load(entry.value());

    let Ok(amount) = rune_entry.mintable(self.height.into()) else {
      return Ok(None);
    };

    drop(entry);

    rune_entry.mints += 1;
    *self.minted.entry(id).or_default() += 1;

    self.id_to_entry.insert(&id.store(), rune_entry.store())?;

    Ok(Some(Lot(amount)))
  }

  fn tx_commits_to_rune(&self, tx: &Transaction, rune: Rune) -> Result<bool> {
    let commitment = rune.commitment();

    for input in &tx.input {
      // extracting a tapscript does not indicate that the input being spent
      // was actually a taproot output. this is checked below, when we load the
      // output's entry from the database
      let Some(tapscript) = input.witness.tapscript() else {
        continue;
      };

      for instruction in tapscript.instructions() {
        // ignore errors, since the extracted script may not be valid
        let Ok(instruction) = instruction else {
          break;
        };

        let Some(pushbytes) = instruction.push_bytes() else {
          continue;
        };

        if pushbytes.as_bytes() != commitment {
          continue;
        }

        let Some(tx_info) = self
          .client
          .get_raw_transaction_info(&input.previous_output.txid, None)
          .into_option()?
        else {
          panic!(
            "can't get input transaction: {}",
            input.previous_output.txid
          );
        };

        let taproot = tx_info.vout[input.previous_output.vout.into_usize()]
          .script_pub_key
          .script()?
          .is_v1_p2tr();

        if !taproot {
          continue;
        }

        let commit_tx_height = self
          .client
          .get_block_header_info(&tx_info.blockhash.unwrap())
          .into_option()?
          .unwrap()
          .height;

        let confirmations = self
          .height
          .checked_sub(commit_tx_height.try_into().unwrap())
          .unwrap()
          + 1;

        if confirmations >= Runestone::COMMIT_CONFIRMATIONS.into() {
          return Ok(true);
        }
      }
    }

    Ok(false)
  }

  fn unallocated(&mut self, tx: &Transaction) -> Result<(HashMap<RuneId, Lot>, HashMap<OutPoint, Vec<(RuneId, u128)>>)> {
    // map of rune ID to un-allocated balance of that rune
    let mut unallocated: HashMap<RuneId, Lot> = HashMap::new();
    let mut tx_inputs: HashMap<OutPoint, Vec<(RuneId, u128)>> = HashMap::new(); // outpoint -> (id, amount)[]

    // increment unallocated runes with the runes in tx inputs
    for input in &tx.input {
      let mut removed = false;
      if let Some(guard) = self
        .outpoint_to_balances
        .remove(&input.previous_output.store())?
      {
        let buffer = guard.value();
        let mut i = 0;
        while i < buffer.len() {
          let ((id, balance), len) = Index::decode_rune_balance(&buffer[i..]).unwrap();
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

    Ok((unallocated, tx_inputs))
  }
}
