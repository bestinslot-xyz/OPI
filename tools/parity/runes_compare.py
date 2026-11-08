#!/usr/bin/env python3
"""Compares a reference runes database with a candidate runes database.

The reference is written by modules/runes_index/index_runes.js (see commit
b3379c5), the candidate by the opi-runes module. Both use
the schema in crates/opi-runes/src/db_init.sql.
Every table is streamed per height through server-side cursors, so memory use
is bounded by one block of rows per side.

Per height in range:
  runes_events                  per txid group in id order. The event type
                                sequence and every event other than input and
                                burn must match in order. Input events and burn
                                events are compared as multisets within the tx.
                                Fields: event_type, txid, outpoint, pkscript,
                                rune_id, amount, wallet_addr. Event ids are not
                                compared. A wallet_addr difference on a p2tr
                                pkscript (input and output events alike) is a
                                warning, on any other it fails.
  runes_outpoint_to_balances    rows created at the height, keyed by outpoint.
  runes_id_to_entry             rows with genesis_height = height, keyed by
                                rune_id. burned, mints and
                                last_updated_block_height differences are
                                warnings, every other column except id fails.
  runes_id_to_entry_changes     rows at the height, keyed by rune_id.
  runes_block_hashes            rows present on both sides.

  runes_cumulative_event_hashes block_event_hash must be equal at heights where
                                both sides have a row and their event sequences
                                are identical in exact order. Heights where input
                                or burn events are reordered skip the check, as
                                do cumulative hashes (the candidate hashes a
                                canonical event order).

As information the tool prints the heights where the reference's block_event_hash
equals the v1-style hash of the reference's own event order.

Exit codes: 0 equal, 1 differences found, 2 usage or connection error.
"""

import argparse
import hashlib
import itertools
import sys
from decimal import Decimal

import psycopg

ITERSIZE = 5000
EVENT_NAMES = {0: "input", 1: "new_rune_allocation", 2: "mint", 3: "output", 4: "burn"}
MULTISET_TYPES = (0, 4)
MUTABLE_ENTRY_COLUMNS = ("burned", "mints", "last_updated_block_height")
ENTRY_COLUMNS = (
  "rune_id", "rune_block", "burned", "divisibility", "etching", "terms_amount", "terms_cap", "terms_height_l", "terms_height_h",
  "terms_offset_l", "terms_offset_h", "mints", "number", "premine", "rune_name", "spacers", "symbol", "timestamp", "turbo",
  "genesis_height", "last_updated_block_height",
)
BALANCE_COLUMNS = ("outpoint", "pkscript", "rune_ids", "balances", "spent", "spent_block_height")
EVENT_COLUMNS = ("event_type", "txid", "outpoint", "pkscript", "rune_id", "amount")
MAX_WARNINGS_PRINTED = 20


class Report:
  def __init__(self):
    self.failures = []  # (height, table, message), first per table
    self.warning_count = 0
    self.warnings_printed = 0

  def warn(self, table, height, message):
    self.warning_count += 1
    if self.warnings_printed < MAX_WARNINGS_PRINTED:
      self.warnings_printed += 1
      print(f"warning: {table} height {height}: {message}")

  def fail(self, table, height, message):
    self.failures.append((height, table, message))
    print(f"DIFFERENCE: {table} height {height}: {message}")


class Difference(Exception):
  def __init__(self, height, message):
    super().__init__(message)
    self.height = height
    self.message = message


def norm(v):
  """Treats NULL and the empty string alike, as the event hash does."""
  return "" if v is None else v


def is_p2tr(pkscript):
  return pkscript is not None and len(pkscript) == 68 and pkscript.startswith("5120")


def stream(conn, name, sql, params):
  cur = conn.cursor(name=name)
  cur.itersize = ITERSIZE
  cur.execute(sql, params)
  return cur


def height_groups(cursor):
  """Yields (height, rows) with rows in cursor order; height is the first column."""
  for height, group in itertools.groupby(cursor, key=lambda r: r[0]):
    yield height, [r[1:] for r in group]


def merge(ref_groups, cand_groups):
  """Yields (height, ref_rows, cand_rows) over the union of heights, ascending."""
  sentinel = (None, None)
  r = next(ref_groups, sentinel)
  c = next(cand_groups, sentinel)
  while r[0] is not None or c[0] is not None:
    if c[0] is None or (r[0] is not None and r[0] < c[0]):
      yield r[0], r[1], []
      r = next(ref_groups, sentinel)
    elif r[0] is None or c[0] < r[0]:
      yield c[0], [], c[1]
      c = next(cand_groups, sentinel)
    else:
      yield r[0], r[1], c[1]
      r = next(ref_groups, sentinel)
      c = next(cand_groups, sentinel)


def fmt(row):
  return "(" + ", ".join(repr(x) if not isinstance(x, Decimal) else str(x) for x in row) + ")"


# runes_events ---------------------------------------------------------------

def tx_groups(rows):
  """rows: (txid, event_type, outpoint, pkscript, wallet_addr, rune_id, amount) in id order."""
  groups = [(txid, list(g)) for txid, g in itertools.groupby(rows, key=lambda r: r[0])]
  seen = set()
  for txid, _ in groups:
    if txid in seen:
      raise ValueError(txid)
    seen.add(txid)
  return groups


def base_key(e):
  txid, typ, outpoint, pkscript, _wallet, rune_id, amount = e
  return (typ, txid, norm(outpoint), norm(pkscript), rune_id, amount)


def sort_key(e):
  return (base_key(e), norm(e[4]))


def compare_wallet(report, height, ref_e, cand_e):
  """Returns a failure message or None; a p2tr wallet difference warns."""
  if norm(ref_e[4]) == norm(cand_e[4]):
    return None
  if is_p2tr(ref_e[3]) and is_p2tr(cand_e[3]):
    report.warn("runes_events", height, f"p2tr wallet_addr differs {fmt(base_key(ref_e))} ref={ref_e[4]!r} cand={cand_e[4]!r}")
    return None
  return f"wallet_addr differs {fmt(base_key(ref_e))} ref={ref_e[4]!r} cand={cand_e[4]!r}"


def compare_event_block(report, height, ref_rows, cand_rows):
  try:
    ref_tx = tx_groups(ref_rows)
  except ValueError as e:
    raise Difference(height, f"reference events of tx {e} are not contiguous in id order")
  try:
    cand_tx = tx_groups(cand_rows)
  except ValueError as e:
    raise Difference(height, f"candidate events of tx {e} are not contiguous in id order")
  for i in range(max(len(ref_tx), len(cand_tx))):
    if i >= len(ref_tx):
      raise Difference(height, f"candidate has an extra tx {cand_tx[i][0]} at tx position {i}")
    if i >= len(cand_tx):
      raise Difference(height, f"candidate is missing tx {ref_tx[i][0]} at tx position {i}")
    (rtx, rev), (ctx, cev) = ref_tx[i], cand_tx[i]
    if rtx != ctx:
      raise Difference(height, f"tx order differs at position {i}: reference {rtx}, candidate {ctx}")
    rtypes = [e[1] for e in rev]
    ctypes = [e[1] for e in cev]
    if rtypes != ctypes:
      raise Difference(height, f"tx {rtx}: event type sequence differs: reference {rtypes}, candidate {ctypes}")
    # ordered events
    rfix = [e for e in rev if e[1] not in MULTISET_TYPES]
    cfix = [e for e in cev if e[1] not in MULTISET_TYPES]
    for n, (re_, ce) in enumerate(zip(rfix, cfix)):
      if base_key(re_) != base_key(ce):
        raise Difference(height, f"tx {rtx}: ordered event {n} differs: reference {fmt(base_key(re_))}, candidate {fmt(base_key(ce))}")
      msg = compare_wallet(report, height, re_, ce)
      if msg:
        raise Difference(height, f"tx {rtx}: {msg}")
    # multiset events
    for typ in MULTISET_TYPES:
      rm = sorted((e for e in rev if e[1] == typ), key=sort_key)
      cm = sorted((e for e in cev if e[1] == typ), key=sort_key)
      for re_, ce in zip(rm, cm):
        if base_key(re_) != base_key(ce):
          raise Difference(
            height,
            f"tx {rtx}: {EVENT_NAMES[typ]} events differ as multisets: reference {fmt(base_key(re_))}, candidate {fmt(base_key(ce))}",
          )
        msg = compare_wallet(report, height, re_, ce)
        if msg:
          raise Difference(height, f"tx {rtx}: {msg}")


EVENTS_SQL = (
  "SELECT block_height, txid, event_type, outpoint, pkscript, wallet_addr, rune_id, amount FROM runes_events "
  "WHERE block_height BETWEEN %s AND %s ORDER BY block_height, id"
)


def check_events(ref, cand, lo, hi, report):
  rg = height_groups(stream(ref, "ev_ref", EVENTS_SQL, (lo, hi)))
  cg = height_groups(stream(cand, "ev_cand", EVENTS_SQL, (lo, hi)))
  for height, rrows, crows in merge(rg, cg):
    compare_event_block(report, height, rrows, crows)


# keyed tables -----------------------------------------------------------------

def compare_keyed(table, height, ref_rows, cand_rows, key_index, columns, report, warn_columns=(), wallet_index=None, pkscript_index=None):
  """Rows are tuples laid out as `columns` (plus an optional wallet column at wallet_index)."""
  rmap = {r[key_index]: r for r in ref_rows}
  cmap = {r[key_index]: r for r in cand_rows}
  if len(rmap) != len(ref_rows):
    raise Difference(height, "the reference has duplicate keys at this height")
  if len(cmap) != len(cand_rows):
    raise Difference(height, "the candidate has duplicate keys at this height")
  for key in sorted(rmap):
    if key not in cmap:
      raise Difference(height, f"row {key!r} is missing from the candidate: {fmt(rmap[key])}")
  for key in sorted(cmap):
    if key not in rmap:
      raise Difference(height, f"row {key!r} is only in the candidate: {fmt(cmap[key])}")
  for key in sorted(rmap):
    rrow, crow = rmap[key], cmap[key]
    for i, col in enumerate(columns):
      if rrow[i] == crow[i]:
        continue
      if col in warn_columns:
        report.warn(table, height, f"row {key!r} column {col} differs: reference {rrow[i]!r}, candidate {crow[i]!r}")
        continue
      raise Difference(height, f"row {key!r} column {col} differs: reference {rrow[i]!r}, candidate {crow[i]!r}")
    if wallet_index is not None and norm(rrow[wallet_index]) != norm(crow[wallet_index]):
      detail = f"row {key!r} wallet_addr differs: reference {rrow[wallet_index]!r}, candidate {crow[wallet_index]!r}"
      if is_p2tr(rrow[pkscript_index]):
        report.warn(table, height, "p2tr " + detail)
      else:
        raise Difference(height, detail)


def check_keyed(ref, cand, lo, hi, report, table, tag, sql, key_index, columns, **kw):
  rg = height_groups(stream(ref, tag + "_ref", sql, (lo, hi)))
  cg = height_groups(stream(cand, tag + "_cand", sql, (lo, hi)))
  for height, rrows, crows in merge(rg, cg):
    compare_keyed(table, height, rrows, crows, key_index, columns, report, **kw)


def check_balances(ref, cand, lo, hi, report):
  sql = (
    "SELECT block_height, outpoint, pkscript, rune_ids, balances, spent, spent_block_height, wallet_addr FROM runes_outpoint_to_balances "
    'WHERE block_height BETWEEN %s AND %s ORDER BY block_height, outpoint COLLATE "C"'
  )
  check_keyed(ref, cand, lo, hi, report, "runes_outpoint_to_balances", "bal", sql, 0, BALANCE_COLUMNS, wallet_index=6, pkscript_index=1)


def check_entries(ref, cand, lo, hi, report):
  cols = ", ".join(f'"{c}"' for c in ENTRY_COLUMNS)
  sql = (
    f"SELECT genesis_height, {cols} FROM runes_id_to_entry "
    'WHERE genesis_height BETWEEN %s AND %s ORDER BY genesis_height, rune_id COLLATE "C"'
  )
  check_keyed(ref, cand, lo, hi, report, "runes_id_to_entry", "ent", sql, 0, ENTRY_COLUMNS, warn_columns=MUTABLE_ENTRY_COLUMNS)


def check_changes(ref, cand, lo, hi, report):
  sql = (
    "SELECT block_height, rune_id, burned, mints FROM runes_id_to_entry_changes "
    'WHERE block_height BETWEEN %s AND %s ORDER BY block_height, rune_id COLLATE "C"'
  )
  check_keyed(ref, cand, lo, hi, report, "runes_id_to_entry_changes", "chg", sql, 0, ("rune_id", "burned", "mints"))


def check_block_hashes(ref, cand, lo, hi, report):
  sql = "SELECT block_height, block_hash, block_time FROM runes_block_hashes WHERE block_height BETWEEN %s AND %s ORDER BY block_height"
  rg = height_groups(stream(ref, "bh_ref", sql, (lo, hi)))
  cg = height_groups(stream(cand, "bh_cand", sql, (lo, hi)))
  for height, rrows, crows in merge(rg, cg):
    if not rrows or not crows:
      continue
    if rrows[0][0] != crows[0][0]:
      raise Difference(height, f"block_hash differs: reference {rrows[0][0]}, candidate {crows[0][0]}")
    if rrows[0][1] != crows[0][1]:
      raise Difference(height, f"block_time differs: reference {rrows[0][1]}, candidate {crows[0][1]}")


class HeightEvents:
  """Event rows per height from one ordered stream; `at` must be called with ascending heights."""

  def __init__(self, conn, name, lo, hi):
    sql = (
      "SELECT block_height, event_type, txid, outpoint, pkscript, rune_id, amount FROM runes_events "
      "WHERE block_height BETWEEN %s AND %s ORDER BY block_height, id"
    )
    self.groups = height_groups(stream(conn, name, sql, (lo, hi)))
    self.pending = next(self.groups, None)

  def at(self, height):
    """Rows (event_type, txid, outpoint, pkscript, rune_id, amount) with NULL outpoint and pkscript as ''."""
    while self.pending is not None and self.pending[0] < height:
      self.pending = next(self.groups, None)
    if self.pending is None or self.pending[0] != height:
      return []
    return [(t, txid, norm(o), norm(p), rid, amt) for t, txid, o, p, rid, amt in self.pending[1]]


def check_event_hashes(ref, cand, lo, hi, report):
  """Heights with identical event sequences on both sides must carry equal block_event_hash.

  Heights where the candidate reordered input or burn events skip the check."""
  hash_sql = "SELECT block_height, block_event_hash FROM runes_cumulative_event_hashes WHERE block_height BETWEEN %s AND %s ORDER BY block_height"
  rg = height_groups(stream(ref, "ch_ref", hash_sql, (lo, hi)))
  cg = height_groups(stream(cand, "ch_cand", hash_sql, (lo, hi)))
  rev = HeightEvents(ref, "che_ref", lo, hi)
  cev = HeightEvents(cand, "che_cand", lo, hi)
  for height, rrows, crows in merge(rg, cg):
    if not rrows or not crows:
      continue
    if rev.at(height) != cev.at(height):
      continue
    if rrows[0][0] != crows[0][0]:
      raise Difference(height, f"block_event_hash differs on an identical event sequence: reference {rrows[0][0]}, candidate {crows[0][0]}")


# information: v1 hash of the reference order ---------------------------------

def event_hash_string(rows):
  """rows: (event_type, outpoint, pkscript, rune_id, amount) in id order."""
  return "|".join(f"{EVENT_NAMES[t]};{norm(o)};{norm(p)};{rid};{amt}" for t, o, p, rid, amt in rows)


def hash_agreement(ref, lo, hi):
  """Returns (matching heights, mismatching heights) of the reference's block_event_hash against the v1-style hash of its own events."""
  ev_sql = (
    "SELECT block_height, event_type, outpoint, pkscript, rune_id, amount FROM runes_events "
    "WHERE block_height BETWEEN %s AND %s ORDER BY block_height, id"
  )
  hash_sql = (
    "SELECT block_height, block_event_hash FROM runes_cumulative_event_hashes "
    "WHERE block_height BETWEEN %s AND %s ORDER BY block_height"
  )
  events = height_groups(stream(ref, "hv_ev", ev_sql, (lo, hi)))
  pending = next(events, None)
  matched, mismatched = [], []
  for height, block_event_hash in stream(ref, "hv_hash", hash_sql, (lo, hi)):
    while pending is not None and pending[0] < height:
      pending = next(events, None)
    rows = pending[1] if pending is not None and pending[0] == height else []
    ours = hashlib.sha256(event_hash_string(rows).encode()).hexdigest()
    (matched if ours == block_event_hash else mismatched).append(height)
  return matched, mismatched


def summarize(heights, limit=20):
  if len(heights) <= limit:
    return " ".join(map(str, heights))
  return " ".join(map(str, heights[:limit])) + f" ... ({len(heights) - limit} more)"


# driver -------------------------------------------------------------------------

CHECKS = (
  ("runes_events", check_events),
  ("runes_outpoint_to_balances", check_balances),
  ("runes_id_to_entry", check_entries),
  ("runes_id_to_entry_changes", check_changes),
  ("runes_block_hashes", check_block_hashes),
  ("runes_cumulative_event_hashes", check_event_hashes),
)


class UsageError(Exception):
  pass


def default_to_height(ref, cand):
  tops = []
  for conn in (ref, cand):
    top = conn.execute("SELECT max(block_height) FROM runes_block_hashes").fetchone()[0]
    if top is None:
      raise UsageError("runes_block_hashes is empty on one side; pass --to-height")
    tops.append(top)
  return min(tops)


def main(argv=None):
  p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
  p.add_argument("--reference", required=True, help="Postgres URL of the reference runes database")
  p.add_argument("--candidate", required=True, help="Postgres URL of the opi-runes database")
  p.add_argument("--from-height", type=int, default=0)
  p.add_argument("--to-height", type=int, default=None, help="default: the lower of the two max(runes_block_hashes.block_height)")
  args = p.parse_args(argv)

  try:
    return run(args)
  except UsageError as e:
    print(f"error: {e}", file=sys.stderr)
    return 2
  except psycopg.Error as e:
    print(f"error: database: {e}", file=sys.stderr)
    return 2


def run(args):
  with psycopg.connect(args.reference) as ref, psycopg.connect(args.candidate) as cand:
    ref.read_only = True
    cand.read_only = True
    lo = args.from_height
    hi = args.to_height if args.to_height is not None else default_to_height(ref, cand)
    print(f"comparing heights {lo}..{hi}")
    report = Report()
    for table, check in CHECKS:
      try:
        check(ref, cand, lo, hi, report)
        print(f"ok: {table}")
      except Difference as d:
        report.fail(table, d.height, d.message)
      ref.rollback()
      cand.rollback()
    matched, mismatched = hash_agreement(ref, lo, hi)
    print(f"info: hash match (reference block_event_hash equals the v1-style hash of the reference order) at {len(matched)} heights: {summarize(matched)}")
    print(f"info: hash mismatch at {len(mismatched)} heights: {summarize(mismatched)}")
    print(f"warnings: {report.warning_count}")
    if report.failures:
      height, table, message = min(report.failures, key=lambda f: f[0])
      print(f"FIRST DIFFERENCE: height {height} table {table}: {message}")
      return 1
    print("EQUAL")
    return 0


if __name__ == "__main__":
  sys.exit(main())
