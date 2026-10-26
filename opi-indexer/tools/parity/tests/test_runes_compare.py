"""Tests for runes_compare.py.

Each test creates two throwaway Postgres databases (reference and candidate)
with the runes schema, fills them with small fixtures, runs the tool as a
subprocess and drops the databases. Needs a local Postgres (127.0.0.1, user
alper, trust auth) and runs only under OPI_TEST_PG=1.
"""

import hashlib
import os
import subprocess
import sys
import uuid
from pathlib import Path

import psycopg
import pytest

HERE = Path(__file__).resolve().parent
TOOL = HERE.parent / "runes_compare.py"
SCHEMA = HERE.parents[3] / "modules" / "runes_index" / "db_init.sql"
if not SCHEMA.exists():
  SCHEMA = next(p for p in HERE.parents if (p / "modules" / "runes_index" / "db_init.sql").exists()) / "modules" / "runes_index" / "db_init.sql"

PG_HOST = os.environ.get("OPI_TEST_PG_HOST", "127.0.0.1")
PG_USER = os.environ.get("OPI_TEST_PG_USER", "alper")

pytestmark = pytest.mark.skipif(os.environ.get("OPI_TEST_PG") != "1", reason="set OPI_TEST_PG=1 to run Postgres-backed tests")

TX1 = "aa" * 32
TX2 = "bb" * 32
OP_A = TX1 + ":0"
OP_B = TX1 + ":1"
P2WPKH = "0014" + "11" * 20
P2TR = "5120" + "22" * 32
HASH_INPUT_A = f"input;{OP_A};{P2WPKH};840000:1;100"


def url(name):
  return f"postgresql://{PG_USER}@{PG_HOST}:5432/{name}"


def admin():
  return psycopg.connect(url("postgres"), autocommit=True)


@pytest.fixture
def dbs():
  names = [f"runes_cmp_{uuid.uuid4().hex[:10]}_{side}" for side in ("ref", "cand")]
  with admin() as a:
    for n in names:
      a.execute(f'CREATE DATABASE "{n}"')
  conns = []
  try:
    for n in names:
      c = psycopg.connect(url(n), autocommit=True)
      c.execute(SCHEMA.read_text())
      conns.append(c)
    yield conns[0], conns[1], names
  finally:
    for c in conns:
      c.close()
    with admin() as a:
      for n in names:
        a.execute(f'DROP DATABASE IF EXISTS "{n}" WITH (FORCE)')


def event(conn, height, typ, txid, outpoint, pkscript, rune_id, amount, wallet=None, eid=None):
  conn.execute(
    "INSERT INTO runes_events (event_type, txid, outpoint, pkscript, wallet_addr, rune_id, amount, block_height) VALUES (%s,%s,%s,%s,%s,%s,%s,%s)",
    (typ, txid, outpoint, pkscript, wallet, rune_id, amount, height),
  )


def balance(conn, outpoint, pkscript, height, rune_ids, balances, wallet=None, spent=False, spent_height=None):
  conn.execute(
    "INSERT INTO runes_outpoint_to_balances (outpoint, pkscript, wallet_addr, rune_ids, balances, block_height, spent, spent_block_height) VALUES (%s,%s,%s,%s,%s,%s,%s,%s)",
    (outpoint, pkscript, wallet, rune_ids, balances, height, spent, spent_height),
  )


def entry(conn, rune_id, height, burned=0, mints=0, last_updated=None, name="AAAA", number=0):
  conn.execute(
    """INSERT INTO runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching, terms_amount, terms_cap, mints, "number",
         premine, rune_name, spacers, symbol, "timestamp", turbo, genesis_height, last_updated_block_height)
       VALUES (%s, %s, %s, 0, %s, 10, 5, %s, %s, 100, %s, 0, 'X', '2024-04-20T00:00:00Z', false, %s, %s)""",
    (rune_id, height, burned, "cc" * 32, mints, number, name, height, last_updated if last_updated is not None else height),
  )


def change(conn, rune_id, height, burned, mints):
  conn.execute("INSERT INTO runes_id_to_entry_changes (rune_id, burned, mints, block_height) VALUES (%s,%s,%s,%s)", (rune_id, burned, mints, height))


def block_hash(conn, height, h="dd"):
  conn.execute("INSERT INTO runes_block_hashes (block_height, block_hash, block_time) VALUES (%s,%s,'2024-04-20T00:00:00Z')", (height, h * 32))


def populate(conn, input_order=("a", "b"), burn_order=("x", "y")):
  """Block 840000 etches 840000:1; block 840001 spends two rune outpoints and burns two runes."""
  entry(conn, "840000:1", 840000)
  event(conn, 840000, 1, TX1, None, None, "840000:1", 100)
  event(conn, 840000, 3, TX1, OP_A, P2WPKH, "840000:1", 60)
  event(conn, 840000, 3, TX1, OP_B, P2TR, "840000:1", 40, wallet="bc1ptest")
  balance(conn, OP_A, P2WPKH, 840000, ["840000:1"], [60], spent=True, spent_height=840001)
  balance(conn, OP_B, P2TR, 840000, ["840000:1"], [40], wallet="bc1ptest", spent=True, spent_height=840001)
  inputs = {"a": (OP_A, P2WPKH, 60), "b": (OP_B, P2TR, 40)}
  for k in input_order:
    op, pk, amt = inputs[k]
    event(conn, 840001, 0, TX2, op, pk, "840000:1", amt, wallet="bc1ptest" if k == "b" else None)
  burns = {"x": ("840000:1", 30), "y": ("840000:2", 5)}
  for k in burn_order:
    rid, amt = burns[k]
    event(conn, 840001, 4, TX2, None, None, rid, amt)
  change(conn, "840000:1", 840001, 30, 0)


def run(names, *extra):
  return subprocess.run(
    [sys.executable, "-I", str(TOOL), "--reference", url(names[0]), "--candidate", url(names[1]), "--from-height", "840000", "--to-height", "840001", *extra],
    capture_output=True,
    text=True,
  )


def test_equal_databases_exit_zero(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr


def test_reordered_inputs_and_burns_within_tx_exit_zero(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand, input_order=("b", "a"), burn_order=("y", "x"))
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr


def test_changed_amount_names_height_and_table(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET amount = amount + 1 WHERE block_height = 840001 AND event_type = 4 AND rune_id = '840000:2'")
  r = run(names)
  assert r.returncode == 1, r.stdout + r.stderr
  assert "840001" in r.stdout and "runes_events" in r.stdout


def test_event_ids_are_not_compared(dbs):
  ref, cand, names = dbs
  populate(ref)
  cand.execute("SELECT setval('runes_events_id_seq', 5000)")
  populate(cand)
  assert run(names).returncode == 0


def test_output_order_change_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET outpoint = %s WHERE block_height = 840000 AND outpoint = %s", ("swap", OP_A))
  cand.execute("UPDATE runes_events SET outpoint = %s WHERE block_height = 840000 AND outpoint = %s", (OP_A, OP_B))
  cand.execute("UPDATE runes_events SET outpoint = %s WHERE block_height = 840000 AND outpoint = %s", (OP_B, "swap"))
  r = run(names)
  assert r.returncode == 1
  assert "840000" in r.stdout and "runes_events" in r.stdout


def test_p2tr_wallet_difference_is_warning(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET wallet_addr = NULL WHERE pkscript = %s", (P2TR,))
  cand.execute("UPDATE runes_outpoint_to_balances SET wallet_addr = NULL WHERE pkscript = %s", (P2TR,))
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr
  assert "warning" in r.stdout.lower()


def test_non_p2tr_wallet_difference_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET wallet_addr = 'bc1other' WHERE pkscript = %s", (P2WPKH,))
  r = run(names)
  assert r.returncode == 1
  assert "runes_events" in r.stdout


def test_balance_difference_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_outpoint_to_balances SET balances = ARRAY[61]::numeric[] WHERE outpoint = %s", (OP_A,))
  r = run(names)
  assert r.returncode == 1
  assert "840000" in r.stdout and "runes_outpoint_to_balances" in r.stdout


def test_missing_balance_row_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("DELETE FROM runes_outpoint_to_balances WHERE outpoint = %s", (OP_B,))
  r = run(names)
  assert r.returncode == 1
  assert "runes_outpoint_to_balances" in r.stdout


def test_entry_mutable_columns_are_warnings(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_id_to_entry SET burned = 30, mints = 2, last_updated_block_height = 840001 WHERE rune_id = '840000:1'")
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr
  assert "warning" in r.stdout.lower() and "runes_id_to_entry" in r.stdout


def test_entry_fixed_column_difference_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_id_to_entry SET premine = 101 WHERE rune_id = '840000:1'")
  r = run(names)
  assert r.returncode == 1
  assert "840000" in r.stdout and "runes_id_to_entry" in r.stdout


def test_entry_changes_difference_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_id_to_entry_changes SET burned = 29")
  r = run(names)
  assert r.returncode == 1
  assert "840001" in r.stdout and "runes_id_to_entry_changes" in r.stdout


def test_first_failing_height_is_reported(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET amount = 61 WHERE block_height = 840000 AND outpoint = %s", (OP_A,))
  cand.execute("UPDATE runes_id_to_entry_changes SET burned = 29")
  r = run(names)
  assert r.returncode == 1
  first = [line for line in r.stdout.splitlines() if line.startswith("FIRST DIFFERENCE")]
  assert first and "840000" in first[0] and "runes_events" in first[0]


def test_heights_outside_range_are_ignored(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  event(cand, 840005, 2, TX2, None, None, "840000:1", 1)
  assert run(names).returncode == 0


def test_block_hash_compared_only_where_both_have_rows(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  block_hash(ref, 840001, "dd")
  block_hash(cand, 840000, "ee")
  block_hash(cand, 840001, "dd")
  assert run(names).returncode == 0
  cand.execute("UPDATE runes_block_hashes SET block_hash = %s WHERE block_height = 840001", ("ff" * 32,))
  r = run(names)
  assert r.returncode == 1
  assert "840001" in r.stdout and "runes_block_hashes" in r.stdout


def test_hash_agreement_heights_are_printed(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  good = hashlib.sha256(f"new_rune_allocation;;;840000:1;100|output;{OP_A};{P2WPKH};840000:1;60|output;{OP_B};{P2TR};840000:1;40".encode()).hexdigest()
  ref.execute("INSERT INTO runes_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (840000, %s, %s)", (good, good))
  ref.execute("INSERT INTO runes_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (840001, %s, %s)", ("00" * 32, "00" * 32))
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr
  assert "hash match" in r.stdout.lower() and "840000" in r.stdout
  assert "hash mismatch" in r.stdout.lower() and "840001" in r.stdout


def test_default_range_uses_common_block_hash_maximum(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  for c in (ref, cand):
    block_hash(c, 840000)
    block_hash(c, 840001)
  cand.execute("UPDATE runes_events SET amount = 61 WHERE block_height = 840001 AND event_type = 0 AND outpoint = %s", (OP_A,))
  r = subprocess.run([sys.executable, "-I", str(TOOL), "--reference", url(names[0]), "--candidate", url(names[1])], capture_output=True, text=True)
  assert r.returncode == 1
  assert "840001" in r.stdout


def replace_tx2_events(conn, spec):
  """Replaces block 840001's events with (type, txid, outpoint, pkscript, rune_id, amount) tuples in order."""
  conn.execute("DELETE FROM runes_events WHERE block_height = 840001")
  for typ, txid, outpoint, pkscript, rune_id, amount in spec:
    event(conn, 840001, typ, txid, outpoint, pkscript, rune_id, amount)


IN_A = (0, TX2, OP_A, P2WPKH, "840000:1", 60)
IN_B = (0, TX2, OP_B, P2TR, "840000:1", 40)
BURN_X = (4, TX2, None, None, "840000:1", 30)
MINT = (2, TX2, None, None, "840000:1", 1)


def assert_events_fail(r):
  assert r.returncode == 1, r.stdout + r.stderr
  assert "runes_events" in r.stdout


def test_output_event_moved_to_another_tx_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("UPDATE runes_events SET txid = %s WHERE block_height = 840000 AND outpoint = %s", (TX2, OP_B))
  assert_events_fail(run(names))


def test_mint_event_moved_to_another_tx_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  event(ref, 840001, 2, TX1, None, None, "840000:1", 1)
  event(cand, 840001, 2, TX2, None, None, "840000:1", 1)
  assert_events_fail(run(names))


def test_tx_events_interleaved_with_another_tx_fail(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  other = (2, TX1, None, None, "840000:1", 1)
  replace_tx2_events(ref, [IN_A, IN_B, BURN_X, other])
  replace_tx2_events(cand, [IN_A, other, IN_B, BURN_X])
  assert_events_fail(run(names))


def test_interleaving_on_both_sides_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  other = (2, TX1, None, None, "840000:1", 1)
  for c in (ref, cand):
    replace_tx2_events(c, [IN_A, other, IN_B, BURN_X])
  assert_events_fail(run(names))


def test_non_input_event_reordered_relative_to_input_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  replace_tx2_events(ref, [IN_A, MINT, IN_B, BURN_X])
  replace_tx2_events(cand, [IN_A, IN_B, MINT, BURN_X])
  assert_events_fail(run(names))


def test_duplicate_input_multiset_difference_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  replace_tx2_events(ref, [IN_A, IN_A, IN_B, BURN_X])
  replace_tx2_events(cand, [IN_A, IN_B, IN_B, BURN_X])
  assert_events_fail(run(names))


def test_duplicate_input_multiset_equal_passes(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  replace_tx2_events(ref, [IN_A, IN_A, IN_B, BURN_X])
  replace_tx2_events(cand, [IN_B, IN_A, IN_A, BURN_X])
  assert run(names).returncode == 0


def add_hashes(ref, cand, height_hashes):
  for conn, hashes in ((ref, height_hashes[0]), (cand, height_hashes[1])):
    for height, h in hashes.items():
      conn.execute("INSERT INTO runes_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (%s,%s,%s)", (height, h, h))


def test_candidate_hash_equal_on_identical_sequences_passes(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  add_hashes(ref, cand, ({840000: "11" * 32, 840001: "22" * 32}, {840000: "11" * 32, 840001: "22" * 32}))
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr


def test_candidate_hash_differs_on_identical_sequence_fails(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  add_hashes(ref, cand, ({840000: "11" * 32, 840001: "22" * 32}, {840000: "11" * 32, 840001: "33" * 32}))
  r = run(names)
  assert r.returncode == 1, r.stdout + r.stderr
  assert "runes_cumulative_event_hashes" in r.stdout and "840001" in r.stdout


def test_candidate_hash_may_differ_when_inputs_reordered(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand, input_order=("b", "a"))
  add_hashes(ref, cand, ({840001: "22" * 32}, {840001: "33" * 32}))
  r = run(names)
  assert r.returncode == 0, r.stdout + r.stderr


def test_candidate_hash_checked_only_where_both_have_rows(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  add_hashes(ref, cand, ({840001: "22" * 32}, {840000: "33" * 32}))
  assert run(names).returncode == 0


def test_duplicate_balance_rows_fail(dbs):
  ref, cand, names = dbs
  populate(ref)
  populate(cand)
  cand.execute("DROP INDEX runes_outpoint_to_balances_outpoint_idx")
  balance(cand, OP_A, P2WPKH, 840000, ["840000:1"], [60], spent=True, spent_height=840001)
  r = run(names)
  assert r.returncode == 1
  assert "runes_outpoint_to_balances" in r.stdout and "duplicate" in r.stdout


def test_missing_default_range_hashes_exits_two(dbs):
  ref, cand, names = dbs
  r = subprocess.run([sys.executable, "-I", str(TOOL), "--reference", url(names[0]), "--candidate", url(names[1])], capture_output=True, text=True)
  assert r.returncode == 2
  assert "Traceback" not in r.stderr


def test_unreachable_database_exits_two(dbs):
  ref, cand, names = dbs
  r = subprocess.run(
    [sys.executable, "-I", str(TOOL), "--reference", url(names[0]), "--candidate", url("runes_cmp_does_not_exist"), "--to-height", "1"],
    capture_output=True,
    text=True,
  )
  assert r.returncode == 2
  assert "Traceback" not in r.stderr
