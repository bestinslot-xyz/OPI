"""Prints the bitmap number parity vectors as JSON.

`get_bitmap_number` is copied verbatim from modules/bitmap_index/bitmap_index.py
at commit 9a1883c (def get_bitmap_number, line 133). `expected` is the decimal string of the
number or null.
"""

import codecs
import json


def get_bitmap_number(content_hex):
  content = None
  try:
    content = codecs.decode(content_hex, "hex").decode('utf-8')
  except:
    pass
  if content is None: return None
  if not content.endswith('.bitmap'): return None
  content = content[:-len('.bitmap')]
  if len(content) == 0: return None
  for ch in content:
    if ord(ch) > ord('9') or ord(ch) < ord('0'):
      return None
  if ord(content[0]) == ord('0') and len(content) != 1: return None
  return int(content)


CORPUS = [
  b"0.bitmap",
  b"00.bitmap",
  b"012.bitmap",
  b"840000.bitmap",
  b"1.bitmap",
  b"123456789.bitmap",
  b".bitmap",
  b"1.bitmap ",
  " 1.bitmap".encode("utf-8"),
  "１.bitmap".encode("utf-8"),
  "1٣.bitmap".encode("utf-8"),
  b"\xff\xfe.bitmap",
  b"1\xff.bitmap",
  b"",
  b"1.BITMAP",
  b"bitmap",
  b"-1.bitmap",
  b"1_0.bitmap",
  b"1.bitmap.bitmap",
  b"1\n.bitmap",
  b"123456789012345678901234567890.bitmap",
  b"18446744073709551615.bitmap",
  b"18446744073709551616.bitmap",
]

vectors = []
for content in CORPUS:
  content_hex = content.hex()
  number = get_bitmap_number(content_hex)
  vectors.append({
    "content_hex": content_hex,
    "expected": None if number is None else str(number),
  })

print(json.dumps(vectors, indent=2))
