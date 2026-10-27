"""Tests for the pure parts of brc20_hash_compare.py (no database needed)."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import brc20_hash_compare as tool  # noqa: E402


def row(height, e="e", ce="ce", t="t", ct="ct"):
  return (height, e, ce, t, ct)


def test_equal_streams():
  r = tool.compare_streams([row(1), row(2)], [row(1), row(2)])
  assert r.equal
  assert r.common == 2 and (r.lo, r.hi) == (1, 2)


def test_each_column_is_reported():
  for i, name in enumerate(tool.HASH_COLUMNS):
    b = list(row(5))
    b[i + 1] = "other"
    r = tool.compare_streams([row(5)], [tuple(b)])
    assert r.mismatches == [(5, [name])]
    assert not r.equal


def test_heights_on_one_side_only_are_not_differences():
  r = tool.compare_streams([row(1), row(2), row(3)], [row(2), row(3), row(4)])
  assert r.only_a == [1] and r.only_b == [4]
  assert r.common == 2 and r.equal


def test_no_common_height_is_not_equal():
  r = tool.compare_streams([row(1)], [row(2)])
  assert r.common == 0 and not r.equal


def test_empty_streams():
  r = tool.compare_streams([], [])
  assert r.common == 0 and not r.equal


def test_summarize_ranges():
  assert tool.summarize([]) == "none"
  assert tool.summarize([3]) == "3"
  assert tool.summarize([1, 2, 3, 7, 9, 10]) == "1-3,7,9-10"
