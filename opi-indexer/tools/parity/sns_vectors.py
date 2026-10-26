"""Prints the SNS parity vectors as JSON.

Run it with Python's json5 0.12.0 installed (the version
modules/sns_index requires):

  python3 -I opi-indexer/tools/parity/sns_vectors.py \
    > opi-indexer/crates/opi-sns/tests/vectors.json

`utf8len`, `get_sns_name` and `get_ns_register` are copied verbatim from
modules/sns_index/sns_index.py (def utf8len line 136, def get_sns_name line
139, def get_ns_register line 184). `outcome` follows index_block's handling of
one inscription (lines 295-326) without the database: the row it would insert,
or "stall" where `utf8len` raises on a lone surrogate, which aborts
index_block for good.

Each vector holds:
- content_hex: the inscription content;
- name, domain, namespace: what get_sns_name / get_ns_register return;
- outcome: null, {"name", "domain"}, {"namespace"} or "stall";
- json5_depth, json5_calls: the deepest frame json5's parser reaches on the
  content, with Parser._grammar_ as 1, and the number of Python calls it
  makes, both without a recursion limit (null for content that is not UTF-8).
Strings that cannot be encoded as UTF-8 (they hold lone surrogates) are lists
of code points.

The vectors run get_sns_name and get_ns_register from the stub
`index_block`, called at module level, so json5's parser runs as many frames
deep as it does in sns_index.py and hits RecursionError at the same nesting.

`hash_chain` holds the block and cumulative event hashes of three
consecutive blocks (the middle one empty), built with EVENT_SEPARATOR,
get_register_event_str, get_ns_register_event_str and get_sha256_hash copied
verbatim from sns_index.py (lines 16, 229, 236 and 242).

`dict_order` is json5.loads of an object with repeated keys as its item list:
`dict(pairs)` keeps each key's first position and its last value.

The file also records `isspace` (every code point with str.isspace) and
`lower` (str.lower of strings that exercise final sigma and multi-character
mappings, then every code point whose lowercase differs).
"""

import codecs
import hashlib
import sys
import unicodedata

try:
  import json5
except ImportError:
  sys.exit('json5 is not installed; run: pip install json5==0.12.0')
import json
from json5 import parser as json5_parser


## helper functions
def utf8len(s):
  return len(s.encode('utf-8'))

def get_sns_name(content_hex):
  content = None
  try:
    content = codecs.decode(content_hex, "hex").decode('utf-8')
  except:
    pass
  if content is None: return [None, None]
  name = content
  try:
    js5 = json5.loads(content)
    if "p" not in js5: return [None, None]
    if "op" not in js5: return [None, None]
    if "name" not in js5: return [None, None]
    if js5["p"] != 'sns': return [None, None]
    if js5["op"] != 'reg': return [None, None]
    name = js5["name"]
    if not isinstance(name, str): return [None, None]
  except:
    pass
  name = name.lower()
  if len(name.split()) == 0: return [None, None]
  name = name.split()[0] # split from whitespace
  name = name.strip()
  if name.count('.') != 1: return [None, None]
  return [name, name.split('.')[1]] ## name, domain

def get_ns_register(content_hex):
  content = None
  try:
    content = codecs.decode(content_hex, "hex").decode('utf-8')
  except:
    pass
  if content is None: return None
  namespace = content
  try:
    js5 = json5.loads(content)
    if "p" not in js5: return None
    if "op" not in js5: return None
    if "ns" not in js5: return None
    if js5["p"] != 'sns': return None
    if js5["op"] != 'ns': return None
    namespace = js5["ns"]
    if not isinstance(namespace, str): return None
  except:
    return None
  namespace = namespace.lower()
  if len(namespace.split()) == 0: return None
  namespace = namespace.split()[0] # split from whitespace
  namespace = namespace.strip()
  if namespace.count('.') != 0: return None
  return namespace


def index_block(content_hex):
  name, domain = get_sns_name(content_hex)
  namespace = get_ns_register(content_hex)
  return name, domain, namespace


def outcome(name, domain, namespace):
  if name is None and namespace is None: return None
  if name is not None:
    if '\x00' in name or '\x00' in domain: return None
    try:
      if utf8len(name) > 2048: return None
    except UnicodeEncodeError:
      return 'stall'
    return {'name': name, 'domain': domain}
  if '\x00' in namespace: return None
  try:
    if utf8len(namespace) > 2048: return None
  except UnicodeEncodeError:
    return 'stall'
  return {'namespace': namespace}


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
  """index_block's event accumulation (lines 264, 312 and 326) and
  update_event_hashes (lines 245-255), with a dict standing in for
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
    cumulative_event_hash = None
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

PARSE_CODE = json5_parser.Parser.parse.__code__


def json5_frames(content_hex):
  try:
    content = codecs.decode(content_hex, "hex").decode('utf-8')
  except UnicodeDecodeError:
    return None, None
  state = {'depth': None, 'max': 0, 'calls': 0}

  def profile(frame, event, arg):
    if event == 'call':
      if state['depth'] is not None:
        state['depth'] += 1
        state['calls'] += 1
        state['max'] = max(state['max'], state['depth'])
      elif frame.f_code is PARSE_CODE:
        state['depth'] = 0
    elif event == 'return' and state['depth'] is not None:
      if frame.f_code is PARSE_CODE:
        state['depth'] = None
      else:
        state['depth'] -= 1

  limit = sys.getrecursionlimit()
  sys.setrecursionlimit(1000000)
  sys.setprofile(profile)
  try:
    json5.loads(content)
  except Exception:
    pass
  finally:
    sys.setprofile(None)
    sys.setrecursionlimit(limit)
  return state['max'], state['calls']


def py_json(s):
  if s is None:
    return None
  if isinstance(s, dict):
    return {k: py_json(v) for k, v in s.items()}
  try:
    s.encode('utf-8')
    return s
  except UnicodeEncodeError:
    return [ord(ch) for ch in s]


def text(s):
  return s.encode('utf-8')


def dumps(obj):
  return json.dumps(obj, ensure_ascii=False)


SNS = 'p:"sns",op:"reg",name:'

CORPUS = []
add = CORPUS.append

# Plain names.
for s in ['alice.sats', 'Alice.SATS', 'alice.sats extra', 'a.b.c', 'alice', '.sats',
          'alice.', '.', '..', 'alice.sats\nmore', ' alice.sats ', '\x1calice.sats\x1c',
          '　alice.sats　', '\x85alice.sats', '\xa0alice.sats\xa0', '\talice.sats\t',
          'a.b\x1dc', 'a.b\x1ec', 'a.b\x1fc', 'a.b᠎c', 'a.b​c', 'a.b﻿c',
          '﻿alice.sats', '', ' ', '\x1c', ' \t\n\r\x0b\x0c', 'alice.sats\x00',
          'ali\x00ce.sats', 'alice.sa\x00ts', 'ünïcödé.säts', '名字.比特币', '🙂.sats',
          '-infinity.5', '1.5', '12', '1.5e3', 'true.x', 'null']:
  add(text(s))
# Every Python whitespace code point around a name.
for cp in range(0x110000):
  if chr(cp).isspace():
    add(text(chr(cp) + 'alice.sats' + chr(cp) + 'x'))
    add(text('a.b' + chr(cp) + 'c.d'))
# Lowercase specials.
for s in ['ΣΑΣ.sats', 'ΑΣ.sats', 'ΑΣ', 'Σ.x', 'ΑΣ\'Β.x', 'Α\'Σ.x', 'ΑΣ.Σ', 'ΑΣ́.x',
          'ΑΣ́', 'x.ΑΣ', 'x.ΑΣΑ', 'İ.sats', 'İ', 'ẞ.x', 'K.x', 'Ⅻ.x', 'ǅ.x',
          'ΑΣ­.x', 'ΑΣ­', 'A­Σ', 'ͅΣ.x', 'aΣ.x', 'AΣ']:
  add(text(s))
# Length limit, measured after lowering.
add(text('a' * 2043 + '.sats'))
add(text('a' * 2044 + '.sats'))
add(text('é' * 1021 + '.sats'))
add(text('é' * 1022 + '.sats'))
add(text('İ' * 1000 + '.x'))
add(text('{"p":"sns","op":"ns","ns":"' + 'a' * 2048 + '"}'))
add(text('{"p":"sns","op":"ns","ns":"' + 'a' * 2049 + '"}'))
# Invalid UTF-8.
for b in [b'\xff.sats', b'\xc0\xae', b'\xed\xa0\x80.x', b'alice.sats\x80', b'\xf4\x90\x80\x80.x',
          b'\xe2\x82.x']:
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
    '{"p":"sns","op":"ns","ns":"SATS more"}',
    '{"p":"sns","op":"ns","ns":"sa.ts"}',
    '{"p":"sns","op":"ns","ns":" "}',
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
]:
  add(text(s))
# Number literals beside a valid registration: a literal json5 cannot convert
# fails the whole document, so the raw content becomes the name.
for lit in ['-0x10', '+0x10', '0x1e', '0X1F', '-0x1e', '-0X1E', '-0xe', '1e', '1e+', '.', '.e1',
            '5.', '.5', '-.5', '5.e3', '01', '1x', '1_0', 'Infinity', '-Infinity', '+Infinity',
            'NaN', '-NaN', 'Infinityx', '0', '-0', '+1', '1E5', '1e-5', '0x', '00',
            '9' * 4300, '9' * 4301, '-' + '9' * 4301, '1' * 5000 + '.0', '0x' + 'f' * 5000]:
  add(text('{"p":"sns","op":"reg","name":"a.b","x":' + lit + '}'))
  add(text('{"p":"sns","op":"ns","ns":"zz","x":' + lit + '}'))
# String escapes and raw characters.
for body in ['a\\x41.b', 'a\\u0041.b', 'a\\a.b', 'a\\/.b', 'a\\é.b', 'a\\1.b', 'a\\01.b',
             'a\\0.b', 'a\\x4.b', 'a\\u004.b', 'a\\\n.b', 'a\\\r\n.b', 'a\\ .b',
             'a\\ .b', 'a\t.b', 'a\n.b', 'a\r.b', 'a .b', 'a .b', 'a\x01.b',
             'a\x1f.b', 'a\x7f.b', 'a\\b\\f\\n\\r\\t\\v.b', 'a\\\'.b', 'a\\".b', 'a\\\\.b',
             '\\ud83d\\ude00.x', 'a.b \\ud800', '\\ud800\\u0000.b', 'x.\\udfff',
             'a.b\\ud800c', '\\ud800']:
  add(text('{"p":"sns","op":"reg","name":"' + body + '"}'))
add(text('{"p":"sns","op":"ns","ns":"\\udc00"}'))
add(text('{"p":"sns","op":"ns","ns":"zz \\udc00"}'))
add(text('{"p":"sns","op":"ns","ns":"\\udc00\\u0000"}'))
add(text('{"\\ud800":1,"p":"sns","op":"reg","name":"k.v"}'))
add(text('{"p":"\\ud800","op":"reg","name":"k.v"}'))
add(text('{"p":"sns","op":"reg","name":\'a"b.c\'}'))
# Values that are not dicts.
for s in ['"alice.sats"', '"pop.name"', '"p op name a.b"', '"p"', "'pop.name'",
          '["p","op","name"]', '["p","op","name","a.b"]', '["p", "op", "name", "a.b"]',
          '["a.b"]', '["p","op","ns","a.b"]', '[["p"],"op","name","a.b"]', '[1.5]',
          '1.5', '12', 'true', 'false', 'null', 'Infinity', '-Infinity', 'NaN', '{}', '[]',
          '{"p":"sns"}', '"\\u0070op.name"', '[\'p\',\'op\',\'name\',"x.y"]',
          '"pop\\u2028name.x"', '"pop.name" ', ' "pop.name"']:
  add(text(s))
# Nesting around Python's recursion limit.
for n in range(54, 62):
  add(text('[' * n + '"a.b"' + ']' * n))
for n in range(42, 50):
  add(text('[1,' * n + '"a.b"' + ']' * n))
for n in range(38, 46):
  add(text('{a:' * n + '"x.y"' + '}' * n))
  add(text('{"a":' * n + '"x.y"' + '}' * n))
for n in range(52, 62):
  add(text('{"p":"sns","op":"reg","name":"q.r","x":' + '[' * n + ']' * n + '}'))
  add(text('{"p":"sns","op":"ns","ns":"zz","x":' + '[' * n + ']' * n + '}'))
LEAVES = ['1', '"a"', '"\\u0041"', '{a:1}', '{\\u0061bé:1}', '/* x */1', '1.5e+3',
          '[1,2]', '{"a":[1]}', '{a:{b:1}}', '0x1F', 'Infinity', 'null', "'a\\\nb'", '[[1]]',
          '{a:[1,{b:2}]}', '　 1', '// c\n1', '-0', '+.5', '"\\x41\\0"', '{a٣:1}',
          '"\\é"', '[{}]', '{}', '[]', '{a:[]}', '[null]']
for n in range(55, 61):
  for leaf in LEAVES:
    add(text('{"p":"sns","op":"reg","name":"q.r","x":' + '[' * n + leaf + ']' * n + '}'))
    add(text('{"p":"sns","op":"ns","ns":"zz","x":' + '[' * n + leaf + ']' * n + '}'))
for n in range(44, 50):
  for leaf in LEAVES:
    add(text('{"p":"sns","op":"reg","name":"q.r","x":' + '[1,' * n + leaf + ']' * n + '}'))

vectors = []
for content in CORPUS:
  content_hex = content.hex()
  name, domain, namespace = index_block(content_hex)
  depth, calls = json5_frames(content_hex)
  vectors.append({
    'content_hex': content_hex,
    'name': py_json(name),
    'domain': py_json(domain),
    'namespace': py_json(namespace),
    'outcome': py_json(outcome(name, domain, namespace)),
    'json5_depth': depth,
    'json5_calls': calls,
  })

LOWER_STRINGS = ['ΣΑΣ', 'ΑΣ', 'Σ', 'ΣΣ', 'ΑΣ\'Β', 'Α\'Σ', 'ΑΣ.Σ', 'ΑΣ́', 'ΑΣ́Β',
                 'ΑΣ­', 'A­Σ', 'ͅΣ', 'aΣ', 'aΣ b', '.Σ', 'Σa', 'ΑΣ\ud800',
                 '\ud800Σ', 'İ', 'İSTANBUL', 'ẞ', 'K', 'Ⅻ', 'ǅ', 'ΑΣΣ', 'ΑΣ1', '1Σ']
lower = [{'s': py_json(s), 'lower': py_json(s.lower())} for s in LOWER_STRINGS]
for cp in range(0x110000):
  if chr(cp).lower() != chr(cp):
    lower.append({'s': chr(cp), 'lower': chr(cp).lower()})

print(dumps({
  'python': sys.version.split()[0],
  'unicode': unicodedata.unidata_version,
  'json5': json5.__version__,
  'recursion_limit': sys.getrecursionlimit(),
  'isspace': [cp for cp in range(0x110000) if chr(cp).isspace()],
  'lower': lower,
  'sns': vectors,
  'hash_chain': hash_chain(HASH_CHAIN_BLOCKS),
  'dict_order': {'content': DICT_ORDER,
                 'items': [list(item) for item in json5.loads(DICT_ORDER).items()]},
}))
