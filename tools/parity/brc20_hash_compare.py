#!/usr/bin/env python3
"""Compares the BRC-20 hash columns of two brc20 databases at every common height.

Both databases hold brc20_cumulative_event_hashes. Per height present in both,
these columns must be equal: block_event_hash, cumulative_event_hash,
block_trace_hash and cumulative_trace_hash. Rows are streamed in height order
through server-side cursors and merged, so memory use is constant.

Heights present on one side only are reported as a count and a range; they are
not differences. The tool fails (exit 1) when any common height differs or when
the two databases have no common height.

Exit codes: 0 equal, 1 difference found, 2 usage or connection error.
"""

import argparse
import sys

import psycopg

ITERSIZE = 20000
HASH_COLUMNS = ("block_event_hash", "cumulative_event_hash", "block_trace_hash", "cumulative_trace_hash")
MAX_MISMATCHES_PRINTED = 20


class UsageError(Exception):
  pass


def diff_columns(row_a, row_b):
  """Names of the hash columns on which two rows (tuples in HASH_COLUMNS order) differ."""
  return [name for name, a, b in zip(HASH_COLUMNS, row_a, row_b) if a != b]


class Result:
  def __init__(self):
    self.common = 0
    self.mismatches = []  # (height, [column names])
    self.only_a = []
    self.only_b = []
    self.lo = None
    self.hi = None

  @property
  def equal(self):
    return self.common > 0 and not self.mismatches


def compare_streams(rows_a, rows_b):
  """Merge-joins two iterators of (height, *hash columns) sorted by height."""
  result = Result()
  it_a, it_b = iter(rows_a), iter(rows_b)
  a, b = next(it_a, None), next(it_b, None)
  while a is not None or b is not None:
    if b is None or (a is not None and a[0] < b[0]):
      result.only_a.append(a[0])
      a = next(it_a, None)
    elif a is None or b[0] < a[0]:
      result.only_b.append(b[0])
      b = next(it_b, None)
    else:
      height = a[0]
      result.common += 1
      if result.lo is None:
        result.lo = height
      result.hi = height
      cols = diff_columns(a[1:], b[1:])
      if cols:
        result.mismatches.append((height, cols))
      a, b = next(it_a, None), next(it_b, None)
  return result


def summarize(heights):
  """Ranges of consecutive heights as 'lo-hi' pieces."""
  if not heights:
    return "none"
  pieces = []
  start = prev = heights[0]
  for h in heights[1:]:
    if h != prev + 1:
      pieces.append(f"{start}-{prev}" if start != prev else str(start))
      start = h
    prev = h
  pieces.append(f"{start}-{prev}" if start != prev else str(start))
  return ",".join(pieces)


def stream(conn, name, lo, hi):
  cur = conn.cursor(name=name)
  cur.itersize = ITERSIZE
  cur.execute(
    f"SELECT block_height, {', '.join(HASH_COLUMNS)} FROM brc20_cumulative_event_hashes "
    "WHERE block_height >= %s AND block_height <= %s ORDER BY block_height",
    (lo, hi),
  )
  return cur


def run(args):
  with psycopg.connect(args.a) as conn_a, psycopg.connect(args.b) as conn_b:
    conn_a.read_only = True
    conn_b.read_only = True
    hi = args.to_height if args.to_height is not None else 2**31 - 1
    result = compare_streams(stream(conn_a, "cmp_a", args.from_height, hi), stream(conn_b, "cmp_b", args.from_height, hi))
  print(f"common heights: {result.common} ({result.lo}..{result.hi})")
  print(f"only in a: {len(result.only_a)} ({summarize(result.only_a)})")
  print(f"only in b: {len(result.only_b)} ({summarize(result.only_b)})")
  print(f"mismatching heights: {len(result.mismatches)}")
  for height, cols in result.mismatches[:MAX_MISMATCHES_PRINTED]:
    print(f"  height {height}: {', '.join(cols)}")
  if result.mismatches:
    print(f"FIRST DIFFERENCE: height {result.mismatches[0][0]}")
    return 1
  if result.common == 0:
    print("no common height", file=sys.stderr)
    return 1
  print("EQUAL")
  return 0


def main(argv=None):
  p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
  p.add_argument("--a", required=True, help="Postgres URL of the first brc20 database")
  p.add_argument("--b", required=True, help="Postgres URL of the second brc20 database")
  p.add_argument("--from-height", type=int, default=0)
  p.add_argument("--to-height", type=int, default=None, help="default: no upper bound")
  args = p.parse_args(argv)
  try:
    return run(args)
  except UsageError as e:
    print(f"error: {e}", file=sys.stderr)
    return 2
  except psycopg.Error as e:
    print(f"error: database: {e}", file=sys.stderr)
    return 2


if __name__ == "__main__":
  sys.exit(main())
