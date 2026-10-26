"""Prints the SNS spec vectors as JSON: an independent implementation of the
SNS indexing rules (opi-indexer/README.md, "SNS indexing rules") applied to a
corpus of inscription contents.

The rules come from the SNS spec
(https://docs.satsnames.org/sats-names/sns-spec/index-names and
https://docs.satsnames.org/sats-names/sns-spec/mint-names) as opi-indexer
reads it. This file shares no code with the Rust implementation:
- JSON5 is Python's json5 0.12.0 package. A document nested deeper than 128
  arrays or objects is a parse failure; it is parsed with Python's recursion
  limit raised, then its nesting depth is checked.
- Lowercasing is `str.lower` (full case mapping with final sigma) of the
  running Python, which must use Unicode 17.0.0.
- Whitespace is WHITE_SPACE below, the White_Space property of Unicode 17.0.0
  PropList.txt.

Run it with the parity venv (json5==0.12.0, Python 3.15):

  python3 -I opi-indexer/tools/parity/sns_spec_vectors.py \
    > opi-indexer/crates/opi-sns/tests/spec_vectors.json

Each vector holds:
- content_hex: the inscription content;
- json5: the json5 result as "error", a canonical value (see `canonical`),
  "deep" for a value too deep to spell out, or "not utf-8";
- outcome: null, {"name", "domain"} or {"namespace"}.

The file also records `white_space`, `lower` (str.lower of strings that
exercise final sigma and multi-character mappings, then every code point whose
lowercase differs), `hash_chain` (event and cumulative hashes of three blocks,
one empty, built with sns_index.py's EVENT_SEPARATOR, get_register_event_str,
get_ns_register_event_str and get_sha256_hash, copied verbatim from
modules/sns_index/sns_index.py lines 16, 229, 236 and 242) and `dict_order`
(json5's object order for repeated keys).
"""

import hashlib
import json
import sys
import unicodedata

try:
  import json5
except ImportError:
  sys.exit('json5 is not installed; run: pip install json5==0.12.0')

if unicodedata.unidata_version != '17.0.0':
  sys.exit(f'Unicode {unicodedata.unidata_version}; the rules use Unicode 17.0.0')

NESTING_LIMIT = 128
MAX_UTF8_LEN = 2048

# White_Space in Unicode 17.0.0 PropList.txt.
WHITE_SPACE = frozenset(
  [0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0x85, 0xA0, 0x1680]
  + list(range(0x2000, 0x200B))
  + [0x2028, 0x2029, 0x202F, 0x205F, 0x3000])


def nesting(value):
  if isinstance(value, dict):
    return 1 + max((nesting(v) for v in value.values()), default=0)
  if isinstance(value, list):
    return 1 + max((nesting(v) for v in value), default=0)
  return 0


def parse_json5(text):
  """The parsed value, or raises on a parse failure."""
  limit = sys.getrecursionlimit()
  sys.setrecursionlimit(200000)
  try:
    value = json5.loads(text)
    depth = nesting(value)
  finally:
    sys.setrecursionlimit(limit)
  if depth > NESTING_LIMIT:
    raise ValueError('nested deeper than %d' % NESTING_LIMIT)
  return value


def is_space(ch):
  return ord(ch) in WHITE_SPACE


def storable(s):
  """No NUL, no lone surrogate, at most 2048 UTF-8 bytes."""
  if '\x00' in s:
    return False
  if any(0xD800 <= ord(ch) <= 0xDFFF for ch in s):
    return False
  return len(s.encode('utf-8')) <= MAX_UTF8_LEN


def sns_object(value, op, key):
  """value[key] when value is an object with p == "sns", op == op and a
  string key, else None."""
  if not isinstance(value, dict):
    return None
  if value.get('p') != 'sns' or value.get('op') != op:
    return None
  found = value.get(key)
  return found if isinstance(found, str) else None


def name_of(text, parsed, value):
  if parsed:
    candidate = sns_object(value, 'reg', 'name')
    if candidate is None:
      return None
  else:
    candidate = text
  candidate = candidate.lower()
  # The first whitespace-separated token: trim leading whitespace, then cut
  # at the first whitespace.
  start = 0
  while start < len(candidate) and is_space(candidate[start]):
    start += 1
  candidate = candidate[start:]
  for i, ch in enumerate(candidate):
    if is_space(ch):
      candidate = candidate[:i]
      break
  if candidate.count('.') != 1:
    return None
  label, namespace = candidate.split('.')
  if label == '' or namespace == '':
    return None
  if not storable(candidate):
    return None
  return {'name': candidate, 'domain': namespace}


def namespace_of(parsed, value):
  if not parsed:
    return None
  namespace = sns_object(value, 'ns', 'ns')
  if namespace is None:
    return None
  namespace = namespace.lower()
  if namespace == '' or '.' in namespace or any(is_space(ch) for ch in namespace):
    return None
  if not storable(namespace):
    return None
  return {'namespace': namespace}


def canonical(value):
  """json5 values in a form JSON can carry: numbers become {"num": true},
  strings with lone surrogates become {"cps": [...]}, objects become
  {"dict": [[key, value], ...]} in dict order."""
  if value is None or isinstance(value, bool):
    return value
  if isinstance(value, (int, float)):
    return {'num': True}
  if isinstance(value, str):
    try:
      value.encode('utf-8')
      return value
    except UnicodeEncodeError:
      return {'cps': [ord(ch) for ch in value]}
  if isinstance(value, list):
    return [canonical(v) for v in value]
  return {'dict': [[canonical(k), canonical(v)] for k, v in value.items()]}


def evaluate(content):
  try:
    text = content.decode('utf-8')
  except UnicodeDecodeError:
    return {'content_hex': content.hex(), 'json5': 'not utf-8', 'outcome': None}
  try:
    value = parse_json5(text)
    parsed = True
  except Exception:
    value, parsed = None, False
  if not parsed:
    shown = 'error'
  elif nesting(value) > 20:
    shown = 'deep'
  else:
    shown = canonical(value)
  outcome = name_of(text, parsed, value) or namespace_of(parsed, value)
  return {'content_hex': content.hex(), 'json5': shown, 'outcome': outcome}


EVENT_SEPARATOR = "|"

def get_register_event_str(name, domain, inscription_id):
  res = "register;"
  res += inscription_id + ";"
  res += name + ";"
  res += domain
  return res

def get_ns_register_event_str(namespace, inscription_id):
  res = "ns_register;"
  res += inscription_id + ";"
  res += namespace
  return res

def get_sha256_hash(s):
  return hashlib.sha256(s.encode('utf-8')).hexdigest()


def hash_chain(blocks):
  """index_block's event accumulation (sns_index.py lines 264, 312 and 326)
  and update_event_hashes (lines 245-255), with a dict standing in for
  sns_names_cumulative_event_hashes."""
  table = {}
  out = []
  for block_height, events in blocks:
    block_events_str = ""
    for event in events:
      if event[0] == 'register':
        _, name, domain, inscr_id = event
        block_events_str += get_register_event_str(str(name), str(domain), str(inscr_id)) + EVENT_SEPARATOR
      else:
        _, namespace, inscr_id = event
        block_events_str += get_ns_register_event_str(str(namespace), str(inscr_id)) + EVENT_SEPARATOR
    if len(block_events_str) > 0 and block_events_str[-1] == EVENT_SEPARATOR: block_events_str = block_events_str[:-1] ## remove last separator
    block_event_hash = get_sha256_hash(block_events_str)
    if (block_height - 1) not in table:
      cumulative_event_hash = block_event_hash
    else:
      cumulative_event_hash = get_sha256_hash(table[block_height - 1] + block_event_hash)
    table[block_height] = cumulative_event_hash
    out.append({
      'block_height': block_height,
      'events': [list(event) for event in events],
      'block_event_hash': block_event_hash,
      'cumulative_event_hash': cumulative_event_hash,
    })
  return out


HASH_CHAIN_BLOCKS = [
  (840000, [('register', 'alice.sats', 'sats', 'aa' * 32 + 'i0'),
            ('ns_register', 'sats', 'bb' * 32 + 'i1'),
            ('register', 'σας.ünï', 'ünï', 'cc' * 32 + 'i0')]),
  (840001, []),
  (840002, [('ns_register', '名字', 'dd' * 32 + 'i0')]),
]

DICT_ORDER = "{b:'1',a:'2',b:'3',c:'4',a:'5',\\u0062:'6',d:'7'}"


def text(s):
  return s.encode('utf-8')


CORPUS = []
add = CORPUS.append

# Spec examples (mint-names, index-names).
SPEC_JSON = ('{\n  "p": "sns",\n  "op": "reg",\n  "name": "satoshi.sats",\n'
             '  "avatar": "7f9c06b712c5b60c0b66868c69568b2d198533553c347cd732fc3c87e62efe86i0",\n'
             '  "rev": "bc1psvggtp7mzr3lu8x5vrycqjqqkk5e7urtu2axjvwejea0ctjfdl3qqrctsf",\n'
             '  "relay": "85b10531435304cbe47d268106b58b57a4416c76573d4b50fa544432597ad670i0"\n}')
for s in ['satoshi.sats', SPEC_JSON, 'Satoshi.SATS', 'a b.sats', 'a.b.c', '1.5', '"x.sats"',
          '{"p":"sns","op":"ns","ns":"my ns"}', '{"p":"sns","op":"ns","ns":"sats"}',
          '{p:"sns",op:"reg",name:"satoshi.sats",}', '{"p":"sns","op":"reg","name":"a\nb.sats"}',
          'satoshi.sats\nsecond line', 'satoshi.gm', 'satoshi.ord',
          # Leading whitespace is trimmed before the cut; only whitespace
          # registers nothing.
          ' alice.sats', '\nalice.sats', '　alice.sats', '\r\n\talice.sats more',
          '{"p":"sns","op":"reg","name":"\\nalice.sats"}', ' ', '\n', '　', ' \n　\t']:
  add(text(s))
# Plain names.
for s in ['alice.sats', 'Alice.SATS', 'alice.sats extra', 'a.b.c', 'alice', '.sats',
          'alice.', '.', '..', 'alice.sats\nmore', ' alice.sats ', 'alice.sats ', '\nalice.sats',
          '\x1calice.sats\x1c', '　alice.sats　', '\x85alice.sats', '\xa0alice.sats\xa0',
          '\talice.sats\t', 'a.b\x1dc', 'a.b\x1ec', 'a.b\x1fc', 'a.b᠎c', 'a.b​c',
          'a.b﻿c', '﻿alice.sats', '', ' ', '\x1c', ' \t\n\r\x0b\x0c', 'alice.sats\x00',
          'ali\x00ce.sats', 'alice.sa\x00ts', 'ünïcödé.säts',
          '名字.比特币', '\U0001f642.sats', '-infinity.5', '12', '1.5e3',
          'true.x', 'null', 'true', 'alice#.sats', 'al?ce.sats', 'a‍.sats']:
  add(text(s))
# Every White_Space code point, and look-alikes, around and inside a name.
for cp in sorted(WHITE_SPACE) + [0x1C, 0x1D, 0x1E, 0x1F, 0x180E, 0x200B, 0x200C, 0x200D,
                                  0x2060, 0xFEFF]:
  ch = chr(cp)
  add(text(ch + 'alice.sats' + ch + 'x'))
  add(text('alice.sats' + ch + 'x'))
  add(text('a.b' + ch + 'c.d'))
  add(text('{"p":"sns","op":"reg","name":"alice.sats' + ch + 'x"}'))
  add(text('{"p":"sns","op":"ns","ns":"sa' + ch + 'ts"}'))
  add(text('{"p":"sns","op":"ns","ns":"' + ch + 'sats"}'))
# Lowercase specials.
for s in ['ΣΑΣ.sats', 'ΑΣ.sats', 'ΑΣ', 'Σ.x',
          "ΑΣ'Β.x", "Α'Σ.x", 'ΑΣ.Σ', 'ΑΣ́.x',
          'x.ΑΣ', 'x.ΑΣΑ', 'İ.sats', 'İ', 'ẞ.x',
          'K.x', 'Ⅻ.x', 'ǅ.x', 'ΑΣ­.x', 'ͅΣ.x', 'aΣ.x',
          'İSTANBUL.SATS']:
  add(text(s))
  add(text('{"p":"sns","op":"ns","ns":"' + s.replace('.', '') + '"}'))
# Length limit, measured after lowercasing.
add(text('a' * 2043 + '.sats'))
add(text('a' * 2044 + '.sats'))
add(text('é' * 1021 + '.sats'))
add(text('é' * 1022 + '.sats'))
add(text('İ' * 1000 + '.x'))
add(text('{"p":"sns","op":"ns","ns":"' + 'a' * 2048 + '"}'))
add(text('{"p":"sns","op":"ns","ns":"' + 'a' * 2049 + '"}'))
add(text('{"p":"sns","op":"ns","ns":"' + 'İ' * 700 + '"}'))
# Invalid UTF-8.
for b in [b'\xff.sats', b'\xc0\xae', b'\xed\xa0\x80.x', b'alice.sats\x80', b'\xf4\x90\x80\x80.x',
          b'\xe2\x82.x', b'{"p":"sns","op":"reg","name":"a.b\xff"}']:
  add(b)
# JSON and JSON5 objects.
for s in [
    '{"p":"sns","op":"reg","name":"alice.sats"}',
    '{"p":"sns","op":"reg","name":"Alice.SATS extra"}',
    '{"p":"sns","op":"reg","name":"  Alice.SATS  extra"}',
    '{"p":"sns","op":"reg","name":"\\u001calice.sats"}',
    '{"p":"sns","op":"reg","name":"a.b\\u0085c"}',
    '{"p":"sns","op":"reg","name":"a.b.c"}',
    '{"p":"sns","op":"reg","name":"alice"}',
    '{"p":"sns","op":"reg","name":""}',
    '{"p":"sns","op":"reg","name":" "}',
    '{"p":"sns","op":"reg","name":".sats"}',
    '{"p":"sns","op":"reg","name":"alice."}',
    '{"op":"reg","name":"alice.sats"}',
    '{"p":"sns","name":"alice.sats"}',
    '{"p":"sns","op":"reg"}',
    '{"p":"SNS","op":"reg","name":"alice.sats"}',
    '{"p":" sns","op":"reg","name":"alice.sats"}',
    '{"p":"sns","op":"REG","name":"alice.sats"}',
    '{"p":null,"op":"reg","name":"alice.sats"}',
    '{"p":"sns","op":"reg","name":123}',
    '{"p":"sns","op":"reg","name":1.5}',
    '{"p":"sns","op":"reg","name":null}',
    '{"p":"sns","op":"reg","name":true}',
    '{"p":"sns","op":"reg","name":["a.b"]}',
    '{"p":"sns","op":"reg","name":{"x":"a.b"}}',
    '{"p":"sns","op":"reg","name":"a.b","ns":"x"}',
    '{"p":"sns","op":"ns","ns":"sats"}',
    '{"p":"sns","op":"ns","ns":"SATS"}',
    '{"p":"sns","op":"ns","ns":"SATS more"}',
    '{"p":"sns","op":"ns","ns":"sa.ts"}',
    '{"p":"sns","op":"ns","ns":"."}',
    '{"p":"sns","op":"ns","ns":""}',
    '{"p":"sns","op":"ns","ns":" "}',
    '{"p":"sns","op":"ns","ns":" sats"}',
    '{"p":"sns","op":"ns","ns":"sats "}',
    '{"p":"sns","op":"ns","ns":"sats\\u001c"}',
    '{"p":"sns","op":"ns","ns":5}',
    '{"p":"sns","op":"ns","ns":"sats","name":"x.y"}',
    '{"p":"sns","op":"ns"}',
    '{"p":"sns","op":"ns","ns":"a\\u0000"}',
    '{"p":"sns","op":"reg","name":"a\\u0000.b"}',
    '{"p":"sns","op":"reg","name":"a.\\u0000b"}',
    '{"p":"sns","op":"reg","name":"\\u0130.x"}',
    '{"p":"sns","op":"reg","name":"\\u03a3\\u0391\\u03a3.x"}',
    '{"p":"x","p":"sns","op":"reg","name":"a.b"}',
    '{"name":"a.b","p":"sns","op":"reg","name":5}',
    '{"p":"sns","op":"reg","name":"a.b","x":{"y":[1,{"z":null}]}}',
    "{p:'sns',op:'reg',name:'alice.sats'}",
    "{'p':'sns','op':'reg','name':'alice.sats'}",
    "{\"p\":'sns','op':\"reg\",name:'it\\'s.x'}",
    '{p:"sns",op:"reg",name:"alice.sats",}',
    '{p:"sns",op:"reg",name:"alice.sats",,}',
    '{,}',
    '/*c*/{p:"sns",op:"reg",name:"alice.sats"}',
    '{p:"sns"//c\n,op:"reg",name:"alice.sats"}',
    '{p:"sns",op:"reg",name:"alice.sats"}//tail',
    '{p:"sns",op:"reg",name:"alice.sats"}/*tail',
    '{p:"sns",op:"reg",name:"alice.sats"} x',
    '{p:"sns",op:"reg",name:"alice.sats"}　',
    '﻿{p:"sns",op:"reg",name:"alice.sats"}',
    '　{p:"sns",op:"reg",name:"alice.sats"}',
    ' {p:"sns",op:"reg",name:"alice.sats"}',
    '\x0b\x0c{p:"sns",op:"reg",name:"alice.sats"}',
    '\x1c{p:"sns",op:"reg",name:"alice.sats"}',
    '\x85{p:"sns",op:"reg",name:"alice.sats"}',
    '᠎{p:"sns",op:"reg",name:"alice.sats"}',
    '{\\u0070:"sns",op:"reg",name:"alice.sats"}',
    '{p\\u0070:1,p:"sns",op:"reg",name:"a.b"}',
    '{\\u0020:1,p:"sns",op:"reg",name:"a.b"}',
    '{$p:1,_:2,p:"sns",op:"reg",name:"a.b"}',
    '{é:1,p:"sns",op:"reg",name:"a.b"}',
    '{ǅ:1,ʰ:2,ℵ:3,ⅰ:4,p:"sns",op:"reg",name:"a.b"}',
    '{a٣:1,á:2,aः:3,a‿:4,a‌‍:5,p:"sns",op:"reg",name:"a.b"}',
    '{٣:1,p:"sns",op:"reg",name:"a.b"}',
    '{1a:1,p:"sns",op:"reg",name:"a.b"}',
    '{a-b:1,p:"sns",op:"reg",name:"a.b"}',
    '{p:"sns",op:"reg",name:"a.b",}x',
    '{p:"sns" op:"reg",name:"a.b"}',
    '{p:"sns",op:"reg",name:"a.b"',
    '{"p":"sns","op":"reg","name":"a.b"}{}',
]:
  add(text(s))
# Number literals beside a valid registration.
for lit in ['-0x10', '+0x10', '0x1e', '0X1F', '-0x1e', '-0X1E', '-0xe', '1e', '1e+', '.', '.e1',
            '5.', '.5', '-.5', '5.e3', '01', '1x', '1_0', 'Infinity', '-Infinity', '+Infinity',
            'NaN', '-NaN', 'Infinityx', '0', '-0', '+1', '1E5', '1e-5', '0x', '00', '1\\u0041',
            '9' * 4300, '9' * 4301, '-' + '9' * 4301, '1' * 5000 + '.0', '0x' + 'f' * 5000,
            'nullx', 'true', 'tru', '[1,]', '[,1]', '[1 2]', '{}', '[]']:
  add(text('{"p":"sns","op":"reg","name":"a.b","x":' + lit + '}'))
  add(text('{"p":"sns","op":"ns","ns":"zz","x":' + lit + '}'))
# String escapes and raw characters.
for body in ['a\\x41.b', 'a\\u0041.b', 'a\\a.b', 'a\\/.b', 'a\\é.b', 'a\\1.b', 'a\\01.b',
             'a\\0.b', 'a\\x4.b', 'a\\u004.b', 'a\\\n.b', 'a\\\r\n.b', 'a\\\r.b', 'a\\ .b',
             'a\\ .b', 'a\t.b', 'a\n.b', 'a\r.b', 'a .b', 'a .b', 'a\x01.b',
             'a\x1f.b', 'a\x7f.b', 'a\\b\\f\\n\\r\\t\\v.b', "a\\'.b", 'a\\".b', 'a\\\\.b',
             '\\ud83d\\ude00.x', 'a.b \\ud800', '\\ud800\\u0000.b', 'x.\\udfff', 'a.b\\ud800c',
             '\\ud800', '\\U0001F642.x', '\\uD83D.x', '\\u00C9.X']:
  add(text('{"p":"sns","op":"reg","name":"' + body + '"}'))
for body in ['\\udc00', 'zz \\udc00', '\\udc00\\u0000', 'z\\ud800z', '\\u0053ATS']:
  add(text('{"p":"sns","op":"ns","ns":"' + body + '"}'))
add(text('{"\\ud800":1,"p":"sns","op":"reg","name":"k.v"}'))
add(text('{"p":"\\ud800","op":"reg","name":"k.v"}'))
add(text('{"p":"sns","op":"reg","name":\'a"b.c\'}'))
# Values that are not objects register nothing.
for s in ['"alice.sats"', '"pop.name"', '"p op name a.b"', '"p"', "'pop.name'",
          '["p","op","name"]', '["p","op","name","a.b"]', '["p", "op", "name", "a.b"]',
          '["a.b"]', '[["p"],"op","name","a.b"]', '[1.5]', '1.5', '12', 'true', 'false', 'null',
          'Infinity', '-Infinity', 'NaN', '{}', '[]', '{"p":"sns"}', '"\\u0070op.name"',
          '"pop\\u2028name.x"', '"pop.name" ', ' "pop.name"', '0x1.5', '-.5', '+1.5', '1.5e+3',
          '[{"p":"sns","op":"reg","name":"a.b"}]']:
  add(text(s))
# Nesting around the limit: 128 levels parse, 129 fail, and a failed parse
# makes the raw text the candidate name.
for n in [1, 2, 60, 127, 128, 129, 130, 200, 1000]:
  add(text('[' * n + '"a.b"' + ']' * n))
  add(text('[' * n + ']' * n))
  add(text('{a:' * n + '"x.y"' + '}' * n))
  add(text('[1,' * n + '"a.b"' + ']' * n))
  add(text('{"p":"sns","op":"reg","name":"q.r","x":' + '[' * (n - 1) + ']' * (n - 1) + '}'))
  add(text('{"p":"sns","op":"ns","ns":"zz","x":' + '[' * (n - 1) + ']' * (n - 1) + '}'))
  add(text('{"p":"sns","op":"reg","name":"q.r","x":' + '{a:' * (n - 1) + '1' + '}' * (n - 1) + '}'))
add(text('[' * 129))
add(text('[' * 129 + '"a.b"'))
add(text('{"p":"sns","op":"reg","name":"q.r","x":"' + '[' * 300 + '"}'))

vectors = [evaluate(content) for content in CORPUS]

LOWER_STRINGS = ['ΣΑΣ', 'ΑΣ', 'Σ', 'ΣΣ',
                 "ΑΣ'Β", "Α'Σ", 'ΑΣ.Σ',
                 'ΑΣ́', 'ΑΣ́Β', 'ΑΣ­',
                 'A­Σ', 'ͅΣ', 'aΣ', 'aΣ b', '.Σ', 'Σa',
                 'İ', 'İSTANBUL', 'ẞ', 'K', 'Ⅻ', 'ǅ',
                 'ΑΣΣ', 'ΑΣ1', '1Σ']
lower = [{'s': s, 'lower': s.lower()} for s in LOWER_STRINGS]
for cp in range(0x110000):
  if 0xD800 <= cp <= 0xDFFF:
    continue
  if chr(cp).lower() != chr(cp):
    lower.append({'s': chr(cp), 'lower': chr(cp).lower()})

print(json.dumps({
  'python': sys.version.split()[0],
  'unicode': unicodedata.unidata_version,
  'json5': json5.__version__,
  'nesting_limit': NESTING_LIMIT,
  'white_space': sorted(WHITE_SPACE),
  'lower': lower,
  'vectors': vectors,
  'hash_chain': hash_chain(HASH_CHAIN_BLOCKS),
  'dict_order': {'content': DICT_ORDER,
                 'items': [list(item) for item in json5.loads(DICT_ORDER).items()]},
}, ensure_ascii=False))
