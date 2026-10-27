#!/usr/bin/env python3
"""Replays one request list against two OPI API servers and diffs status,
Content-Type and body.

Two pairs are supported.

`--pair old-new` (default) compares the four per-module Express apps with the
merged API. Every route of every module is requested with realistic
parameters, then with bad parameters. A request passes when old and new answer
with the same status, Content-Type and body (JSON bodies compare as values).
Differences that are documented behaviour changes sit in ALLOWED and are
reported separately; any other difference makes the exit code 1.

    python3 tools/parity/api_compare.py \\
        --brc20 http://127.0.0.1:8000 --bitmap http://127.0.0.1:8001 \\
        --sns http://127.0.0.1:8002 --runes http://127.0.0.1:8003 \\
        --new http://127.0.0.1:8010

`--pair js-rust` compares the merged Express app (`--js`) with the Rust app
(`opi-indexer api`, `--rust`) that serves the same routes. ALLOWED does not
apply: every difference is a DIFF. The request list adds the quirk cases
(missing, repeated, array and object parameters, loose comparisons, numbers
in every spelling, NUL bytes, invalid percent escapes, very long values,
routing and CORS variants). Bodies compare as exact text and every response
header except Date, Connection, Keep-Alive, X-Powered-By and ETag compares
too.

    python3 tools/parity/api_compare.py --pair js-rust \\
        --js http://127.0.0.1:18200 --rust http://127.0.0.1:18201

Two answers can differ only in the row order of a route whose SQL has no
total order (see UNORDERED_ROUTES). Such a pair is reported as ORDER, not
DIFF, when the rows form the same multiset. `/v1/status` lists modules in
completion order, so its `modules` object compares without key order.

`--rate-limit` replays one fixed request sequence against each server in turn
(start both fresh, with the same RATE_LIMIT_* settings) and compares the
answers position by position, RateLimit headers included.

The default parameter values are signet values (block height, pkscript,
ticker, rune, ...). Each can be overridden on the command line when the
databases hold other data. Run the comparison on databases that are not being
written to, so both sides see the same rows.
"""

import argparse
import json
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Callable, Optional
from urllib.parse import urlencode

MODULES = ("brc20", "bitmap", "sns", "runes")
# The module of requests that belong to no module (status, routing, CORS).
APP = "app"

DEFAULT_BASES = {
    "brc20": "http://127.0.0.1:8000",
    "bitmap": "http://127.0.0.1:8001",
    "sns": "http://127.0.0.1:8002",
    "runes": "http://127.0.0.1:8003",
}
DEFAULT_NEW = "http://127.0.0.1:8010"
DEFAULT_JS = "http://127.0.0.1:18200"
DEFAULT_RUST = "http://127.0.0.1:18201"

# Routes whose SQL has no total order: a grouped query with no ORDER BY, or an
# ORDER BY on a column with ties. Plan changes can reorder their rows in either
# app, so an exact difference that is only a row order is reported as ORDER.
UNORDERED_ROUTES = frozenset(
    {
        "/v1/sns/get_inscriptions_of_domain",
        "/v1/sns/get_registered_namespaces",
        "/v1/runes/balance_on_block",
        "/v1/runes/get_current_balance_of_wallet",
        "/v1/runes/get_unspent_rune_outpoints_of_wallet",
        "/v1/runes/holders",
        "/v1/brc20/get_valid_tx_notes_of_wallet",
        "/v1/brc20/get_valid_tx_notes_of_ticker",
        "/v1/brc20/holders",
        "/v1/brc20/explorer/balances",
    }
)

# Response headers that are not compared: the ones that differ between two runs
# of the same server, and X-Powered-By and ETag, which the Rust app does not
# send by design (clients can not depend on them).
VOLATILE_HEADERS = frozenset({"date", "connection", "keep-alive", "x-powered-by", "etag"})

# Parameter name -> (default, help). The defaults are values present in the
# signet databases.
PARAMS = {
    "height": ("328068", "a block height with indexed activity"),
    "pkscript": (
        "51204230a3d3b78b357fd736ff1763817bfeb5afd284c23868bf951ad5d15c5f4850",
        "a pkscript holding runes",
    ),
    "address": (
        "tb1pggc285ah3v6hl4eklutk8qtml666l55ycgux30u4rt2azhzlfpgqfjjmku",
        "the address of that pkscript",
    ),
    "ticker": ("ordi17", "a BRC-20 ticker"),
    "brc20_pkscript": ("6a09425243323050524f47", "a pkscript with BRC-20 balances"),
    "inscription_id": (
        "65e07976479287e89f6086913cea9a4166c11da2323455c830fe49df3ef8c2e2i0",
        "a BRC-20 event inscription id",
    ),
    "bitmap_number": ("328008", "an inscribed bitmap number"),
    "sns_name": ("test.sats", "an SNS name"),
    "sns_domain": ("test", "an SNS domain"),
    "rune_id": ("328068:150", "a rune id"),
    "rune_name": ("BTCPANELSIGNETTEST", "a rune name"),
    "txid": (
        "2b7a7ba5aba8c5e9ca314fcba04a99ff67689aa637cd2ae8533a75c81c85ab9a",
        "a txid with rune events",
    ),
    "evm_address": ("0x0000000000000000000000000000000000000001", "an EVM address"),
}


@dataclass
class Request:
    """One replayed request. `path` is the same on both servers.

    `query` is a dict that urlencode turns into the query string; `raw_query`
    is sent as is (already percent-encoded), for repeated keys, bracket
    forms and invalid escapes.
    """

    module: str
    path: str
    query: Optional[dict] = None
    kind: str = "good"  # "good", "bad" or "quirk"
    note: str = ""
    # The old handler throws outside any error handling, which ends the old
    # process. Such requests run after all others.
    crash_prone: bool = False
    raw_query: Optional[str] = None
    method: str = "GET"
    headers: Optional[dict] = None

    @property
    def route(self) -> str:
        """The route the request belongs to: lower case, one trailing slash
        removed, the explorer path parameter left out."""
        path = self.path.split("?")[0].lower()
        if path.endswith("/") and len(path) > 1:
            path = path[:-1]
        parts = path.split("/")
        if len(parts) > 4 and parts[3] == "explorer":
            return "/".join(parts[:5])
        return path

    def target(self) -> str:
        if self.raw_query is not None:
            return self.path + "?" + self.raw_query
        if self.query:
            return self.path + "?" + urlencode(self.query)
        return self.path

    def label(self) -> str:
        text = self.target()
        if len(text) > 200:
            text = text[:200] + f"...({len(text)} chars)"
        if self.method != "GET":
            text = f"{self.method} {text}"
        if self.note:
            text += f"  [{self.note}]"
        return text


@dataclass
class Response:
    """A server answer. Status 0 means no HTTP answer (connection error)."""

    status: int
    body: Any
    raw: str = ""
    content_type: str = ""
    headers: dict = field(default_factory=dict)


def parse_response(
    status: int, raw: str, content_type: str = "", headers: Optional[dict] = None
) -> Response:
    """Builds a Response, decoding the body as JSON when it parses."""
    try:
        body = json.loads(raw)
    except ValueError:
        body = raw
    return Response(
        status=status,
        body=body,
        raw=raw,
        content_type=content_type,
        headers=dict(headers or {}),
    )


def diff_responses(old: Response, new: Response) -> Optional[str]:
    """Returns None when both answers match, else a one-line description."""
    if old.status != new.status:
        return f"status old={old.status} new={new.status}"
    if old.content_type != new.content_type:
        return f"content-type old={old.content_type!r} new={new.content_type!r}"
    if old.body != new.body:
        return f"body old={clip(old.raw)} new={clip(new.raw)}"
    return None


def clip(text: str, limit: int = 160) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit] + "..."


@dataclass
class AllowRule:
    """A documented difference between the old and the new API.

    `applies` receives the old and the new answer and says whether this
    difference is the documented one, so an unrelated difference on the same
    route still fails the run.
    """

    route: str
    reason: str
    applies: Callable[[Response, Response], bool]


def _is_empty_object(resp: Response) -> bool:
    return resp.status == 200 and resp.body == {}


def _is_height_answer(resp: Response) -> bool:
    body = resp.body
    if resp.status != 200:
        return False
    if isinstance(body, int):
        return True
    return isinstance(body, str) and body.strip().isdigit()


def _old_unreachable_or_failed(old: Response, new: Response) -> bool:
    return (old.status == 0 or old.status >= 500) and new.status == 200


def _ip_differs(old: Response, new: Response) -> bool:
    return old.status == 200 and new.status == 200


ALLOWED = [
    AllowRule(
        "/v1/bitmap/block_height",
        "R4: the old route answered `{}`, the merged API answers the indexed height",
        lambda old, new: _is_empty_object(old) and _is_height_answer(new),
    ),
    AllowRule(
        "/v1/sns/block_height",
        "R4: the old route answered `{}`, the merged API answers the indexed height",
        lambda old, new: _is_empty_object(old) and _is_height_answer(new),
    ),
    AllowRule(
        "/v1/sns/get_registered_namespaces",
        "R5: the old handler has no error handling and crashes the process; "
        "the merged API answers",
        _old_unreachable_or_failed,
    ),
]
# Routes whose old handler reads the block height from the route parameters,
# where it is always undefined, so the old process crashes; the merged API
# reads it from the query string and answers 200.
for _route in (
    "/v1/bitmap/get_hash_of_all_activity",
    "/v1/sns/get_hash_of_all_activity",
):
    ALLOWED.append(
        AllowRule(
            _route,
            "R6: the old handler reads the block height from the route parameters, "
            "where it is always undefined, and crashes; the merged API reads the "
            "query string and answers 200",
            _old_unreachable_or_failed,
        )
    )
# Routes whose old handler throws outside any error handling on some inputs
# (an async rejection ends the old process); the merged API answers 500.
for _route in (
    "/v1/bitmap/get_hash_of_all_bitmaps",
    "/v1/bitmap/get_inscription_id_of_bitmap",
):
    ALLOWED.append(
        AllowRule(
            _route,
            "R5: the old handler has no error handling and an unhandled rejection "
            "ends the process; the merged API answers 500",
            lambda old, new: old.status == 0 and new.status == 500,
        )
    )
for _module in MODULES:
    ALLOWED.append(
        AllowRule(
            f"/v1/{_module}/ip",
            "the reported client address depends on how each server is reached",
            _ip_differs,
        )
    )


def match_allowed(
    route: str, old: Response, new: Response, rules: list = ALLOWED
) -> Optional[str]:
    """Returns the reason of the first rule that explains the difference."""
    for rule in rules:
        if rule.route == route and rule.applies(old, new):
            return rule.reason
    return None


def classify(request: Request, old: Response, new: Response, rules: list = ALLOWED):
    """Returns (verdict, detail) with verdict PASS, ALLOWED or DIFF."""
    difference = diff_responses(old, new)
    if difference is None:
        return "PASS", ""
    reason = match_allowed(request.route, old, new, rules)
    if reason is not None:
        return "ALLOWED", f"{reason} ({difference})"
    return "DIFF", difference


def canonical_rows(value: Any) -> Any:
    """Sorts every list of objects (rows) by its canonical JSON text, so two
    values compare equal when they hold the same rows in any order. Lists of
    scalars keep their order, which keeps parallel arrays aligned."""
    if isinstance(value, dict):
        return {key: canonical_rows(item) for key, item in value.items()}
    if isinstance(value, list):
        items = [canonical_rows(item) for item in value]
        if any(isinstance(item, dict) for item in items):
            items.sort(key=lambda item: json.dumps(item, sort_keys=True))
        return items
    return value


def comparable_headers(resp: Response) -> dict:
    return {k: v for k, v in resp.headers.items() if k not in VOLATILE_HEADERS}


def status_bodies_match(js: Response, rust: Response) -> bool:
    """`/v1/status` bodies match when everything but the order of the keys of
    `modules` matches."""
    a, b = js.body, rust.body
    # The answer is {"error": ..., "result": {"modules": {...}}}; a bare
    # {"modules": {...}} is read the same way.
    for holder_a, holder_b in ((a, b), (a.get("result") if isinstance(a, dict) else None, b.get("result") if isinstance(b, dict) else None)):
        if not (isinstance(holder_a, dict) and isinstance(holder_b, dict)):
            continue
        if not (isinstance(holder_a.get("modules"), dict) and isinstance(holder_b.get("modules"), dict)):
            continue
        if holder_a["modules"] != holder_b["modules"]:
            return False
        if holder_a is a:
            return list(a.keys()) == list(b.keys()) and json.dumps(
                {k: v for k, v in a.items() if k != "modules"}
            ) == json.dumps({k: v for k, v in b.items() if k != "modules"})
        outer_a = {k: v for k, v in a.items() if k != "result"}
        outer_b = {k: v for k, v in b.items() if k != "result"}
        inner_a = {k: v for k, v in holder_a.items() if k != "modules"}
        inner_b = {k: v for k, v in holder_b.items() if k != "modules"}
        return (
            list(a.keys()) == list(b.keys())
            and list(holder_a.keys()) == list(holder_b.keys())
            and json.dumps(outer_a) == json.dumps(outer_b)
            and json.dumps(inner_a) == json.dumps(inner_b)
        )
    return js.raw == rust.raw


def classify_pair(request: Request, js: Response, rust: Response):
    """Returns (verdict, detail) with verdict PASS, ORDER or DIFF. No rule
    excuses a difference."""
    if js.status != rust.status:
        return "DIFF", f"status js={js.status} rust={rust.status}"
    if js.content_type != rust.content_type:
        return "DIFF", f"content-type js={js.content_type!r} rust={rust.content_type!r}"
    js_headers, rust_headers = comparable_headers(js), comparable_headers(rust)
    if js_headers != rust_headers:
        names = sorted(
            name
            for name in set(js_headers) | set(rust_headers)
            if js_headers.get(name) != rust_headers.get(name)
        )
        parts = [
            f"{name}: js={js_headers.get(name)!r} rust={rust_headers.get(name)!r}"
            for name in names
        ]
        return "DIFF", "headers " + "; ".join(parts)
    if js.raw == rust.raw:
        return "PASS", ""
    if request.route == "/v1/status" and status_bodies_match(js, rust):
        return "PASS", ""
    detail = f"body js={clip(js.raw)} rust={clip(rust.raw)}"
    if (
        request.route in UNORDERED_ROUTES
        and isinstance(js.body, (dict, list))
        and json.dumps(canonical_rows(js.body)) == json.dumps(canonical_rows(rust.body))
    ):
        return "ORDER", "same rows in another order"
    return "DIFF", detail


# Raw query values for a number the routes read, in every spelling the loose
# JS comparisons and Postgres integer input treat differently.
NUMBER_VALUES = [
    "",
    "%20328068",
    "328068%20",
    "%09328068",
    "%2B328068",
    "+328068",
    "0328068",
    "3.28068e5",
    "328068.0",
    "328068.5",
    "1_0",
    "0x10",
    "0b11",
    "0o7",
    "NaN",
    "Infinity",
    "-Infinity",
    "-1",
    "-0",
    "1e21",
    "9999999999",
    "2147483647",
    "2147483648",
    "abc",
    "328068abc",
    "null",
    "undefined",
    "true",
    "%00",
    "%zz",
    "%F0%9F",
    "%E2%9C%93",
    "9" * 5000,
]

# Raw query values for a text parameter.
TEXT_VALUES = [
    "",
    "%20",
    "%25",
    "%25%25",
    "%27",
    "%22",
    "%5C",
    "%3B--",
    "a%2Bb",
    "a+b",
    "%C4%B0",  # dotted capital I, whose lower case has two code points
    "%C3%9F",  # sharp s, whose upper case has two letters
    "%F0%9F%98%80",
    "%00",
    "%zz",
    "%E0%A4%A",
    "1e3",
    "null",
    "undefined",
    "0",
    "x" * 5000,
]


def shapes(name: str) -> list:
    """Raw query strings that give `name` a non-string value or no value: a
    missing key, bare key, repeated key, arrays, objects and key-name
    collisions with Object.prototype."""
    return [
        "",
        name,
        f"{name}=",
        f"{name}=1&{name}=2",
        f"{name}[]=1",
        f"{name}[]=",
        f"{name}[]=1&{name}[]=2",
        f"{name}[]=1&{name}=2",
        f"{name}=1&{name}[]=2",
        f"{name}[a]=1",
        f"{name}[a][b][c][d][e][f]=1",
        f"{name}[toString]=x",
        f"{name}[valueOf]=1",
        f"{name}[__proto__]=x",
        f"{name}[hasOwnProperty]=x",
        f"{name}[0]=1",
        f"{name}[21]=1",
        f"{name}%5B%5D=1",
        f"{name}[%5D=1",
        f"%{name}=1",
    ]


def join(*parts: str) -> str:
    return "&".join(part for part in parts if part != "")


def build_quirk_requests(p: dict) -> list:
    """Requests that probe the JS semantics each route reproduces: parameter
    shapes, number spellings, text edge cases, path and routing variants and
    CORS. Each gets kind "quirk"."""
    out = []

    def add(module, path, raw=None, note="", method="GET", headers=None):
        out.append(
            Request(
                module,
                path,
                kind="quirk",
                note=note,
                raw_query=raw,
                method=method,
                headers=headers,
            )
        )

    h = p["height"]
    bp = p["brc20_pkscript"]
    tk = p["ticker"]
    pk = p["pkscript"]
    ad = p["address"]

    # Block height: every route that compares it with the indexed height.
    height_routes = [
        ("brc20", "/v1/brc20/balance_on_block", join(f"pkscript={bp}", f"ticker={tk}")),
        ("brc20", "/v1/brc20/activity_on_block", ""),
        ("brc20", "/v1/brc20/bitcoin_rpc_results_on_block", ""),
        ("brc20", "/v1/brc20/get_hash_of_all_activity", ""),
        ("bitmap", "/v1/bitmap/get_hash_of_all_activity", ""),
        ("sns", "/v1/sns/get_hash_of_all_activity", ""),
        ("runes", "/v1/runes/get_hash_of_all_activity", ""),
        ("runes", "/v1/runes/activity_on_block", ""),
        ("runes", "/v1/runes/balance_on_block", f"pkscript={pk}"),
    ]
    for module, path, rest in height_routes:
        for value in NUMBER_VALUES:
            add(module, path, join(f"block_height={value}", rest), "height value")
        for shape in shapes("block_height"):
            add(module, path, join(shape, rest), "height shape")

    # Ticker and rune name: toLowerCase / toUpperCase on a non-string throws.
    ticker_routes = [
        ("/v1/brc20/balance_on_block", join(f"block_height={h}", f"pkscript={bp}")),
        ("/v1/brc20/get_current_balance_of_wallet", f"pkscript={bp}"),
        ("/v1/brc20/get_valid_tx_notes_of_ticker", ""),
        ("/v1/brc20/holders", ""),
    ]
    for path, rest in ticker_routes:
        for value in TEXT_VALUES + ["ORDI17", "Ordi17", f"{tk}%20", f"%20{tk}", f"{tk}&{tk}"]:
            add("brc20", path, join(f"ticker={value}", rest), "ticker value")
        for shape in shapes("ticker"):
            add("brc20", path, join(shape, rest), "ticker shape")
    for value in TEXT_VALUES + ["btcpanelsignettest", f"{p['rune_name']}%20"]:
        add("runes", "/v1/runes/holders", f"rune_name={value}", "rune_name value")
        add("runes", "/v1/runes/holders", join(f"rune_name={value}", f"rune_id={p['rune_id']}"), "rune_name value with id")
    for shape in shapes("rune_name"):
        add("runes", "/v1/runes/holders", shape, "rune_name shape")
        add("runes", "/v1/runes/holders", join(shape, f"rune_id={p['rune_id']}"), "rune_name shape with id")
    for shape in shapes("rune_id"):
        add("runes", "/v1/runes/holders", join(f"rune_name={p['rune_name']}", shape), "rune_id shape")
        add("runes", "/v1/runes/holders", join("rune_name=", shape), "rune_id shape, empty name")
    for value in TEXT_VALUES:
        add("runes", "/v1/runes/holders", join("rune_name=", f"rune_id={value}"), "rune_id value, empty name")

    # Wallet selectors: `address || ''`, `pkscript || ''` and the loose
    # `address != ''` pick the column to filter on.
    wallet_routes = [
        ("brc20", "/v1/brc20/get_current_balance_of_wallet", f"ticker={tk}"),
        ("brc20", "/v1/brc20/get_valid_tx_notes_of_wallet", ""),
        ("runes", "/v1/runes/balance_on_block", f"block_height={h}"),
        ("runes", "/v1/runes/get_current_balance_of_wallet", ""),
        ("runes", "/v1/runes/get_unspent_rune_outpoints_of_wallet", ""),
    ]
    for module, path, rest in wallet_routes:
        script = bp if module == "brc20" else pk
        for name in ("pkscript", "address"):
            other = ad if name == "pkscript" else script
            other_name = "address" if name == "pkscript" else "pkscript"
            for value in TEXT_VALUES:
                add(module, path, join(f"{name}={value}", rest), f"{name} value")
                add(module, path, join(f"{name}={value}", f"{other_name}={other}", rest), f"{name} value with {other_name}")
            for shape in shapes(name):
                add(module, path, join(shape, rest), f"{name} shape")
                add(module, path, join(shape, f"{other_name}={other}", rest), f"{name} shape with {other_name}")
        add(module, path, join(f"pkscript={script}", f"address={ad}", rest), "both selectors")
        add(module, path, join("pkscript=", "address=", rest), "both empty")
        add(module, path, join(f"address={ad}", f"pkscript={script}", rest), "address first")

    # Ids and names with a single text parameter.
    single = [
        ("brc20", "/v1/brc20/event", "inscription_id", p["inscription_id"]),
        ("runes", "/v1/runes/event", "transaction_id", p["txid"]),
        ("sns", "/v1/sns/get_info_of_sns", "name", p["sns_name"]),
        ("sns", "/v1/sns/get_inscriptions_of_domain", "domain", p["sns_domain"]),
        ("bitmap", "/v1/bitmap/get_inscription_id_of_bitmap", "bitmap_number", p["bitmap_number"]),
    ]
    for module, path, name, good in single:
        values = list(TEXT_VALUES) + [good.upper(), f"{good}%20", f"%20{good}", f"{good}&{name}={good}"]
        if module == "bitmap":
            values = list(NUMBER_VALUES) + ["0", "1"]
        for value in values:
            add(module, path, f"{name}={value}", f"{name} value")
        for shape in shapes(name):
            add(module, path, shape, f"{name} shape")

    # Explorer tokens: typeof q, parseInt(items_count || '0') || 0,
    # String(order).toLowerCase() === 'asc', '%' + q + '%'.
    for value in TEXT_VALUES + ["ord", "%25", "_", "ORD"]:
        add("brc20", "/v1/brc20/explorer/tokens", join(f"q={value}", "order=asc"), "tokens q value")
    for shape in shapes("q"):
        add("brc20", "/v1/brc20/explorer/tokens", join(shape, "order=asc"), "tokens q shape")
    for value in NUMBER_VALUES + ["3.9", "100", "1e3", "5%20", "%205"]:
        add("brc20", "/v1/brc20/explorer/tokens", join("q=ord", f"items_count={value}", "order=asc"), "tokens items_count value")
    for shape in shapes("items_count"):
        add("brc20", "/v1/brc20/explorer/tokens", join("q=ord", shape, "order=asc"), "tokens items_count shape")
    for value in ["ASC", "Asc", "asc%20", "%20asc", "desc", "DESC", "", "up", "1", "null", "%00", "%zz", "asc&order=desc", "desc&order=asc"]:
        add("brc20", "/v1/brc20/explorer/tokens", join("q=ord", f"order={value}"), "tokens order value")
    for shape in shapes("order"):
        add("brc20", "/v1/brc20/explorer/tokens", join("q=ord", shape), "tokens order shape")
    add("brc20", "/v1/brc20/explorer/tokens", None, "tokens no params")
    add("brc20", "/v1/brc20/explorer/tokens", "q=ord&items_count=0", "tokens no order")
    add("brc20", "/v1/brc20/explorer/tokens", "q=ord&items_count=50&order=asc", "tokens second page")
    add("brc20", "/v1/brc20/explorer/tokens", "q=ord&items_count=50&order=desc", "tokens second page desc")
    add("brc20", "/v1/brc20/explorer/tokens", "q=&items_count=0&order=desc", "tokens empty q")

    # Explorer path parameters: decodeURIComponent of the raw segment.
    segments = [
        p["evm_address"],
        p["evm_address"].upper().replace("0X", "0x"),
        "notanaddress",
        "",
        "%zz",
        "%00",
        "%F0%9F",
        "%E0%A4%A",
        "a%2Fb",
        "%25",
        "%2541",
        "%C4%B0",
        "..",
        "x" * 5000,
        f"{p['evm_address']}/extra",
        f"{p['evm_address']}/",
    ]
    for name in ("btc_address", "balances", "address_tabs_counters"):
        for segment in segments:
            add("brc20", f"/v1/brc20/explorer/{name}/{segment}", None, "explorer path segment")
    add("brc20", "/v1/brc20/explorer", None, "explorer prefix")
    add("brc20", "/v1/brc20/explorer/", None, "explorer prefix slash")
    add("brc20", "/v1/brc20/explorer/nothing", None, "explorer unknown")

    # Routing: case, trailing slash, double slash, prefixes, escapes.
    base_paths = [
        "/v1/brc20/ip",
        "/v1/brc20/block_height",
        "/v1/bitmap/ip",
        "/v1/sns/block_height",
        "/v1/runes/ip",
        "/v1/status",
    ]
    for path in base_paths:
        module = path.split("/")[2] if path.split("/")[2] in MODULES else APP
        for variant, note in (
            (path.upper(), "upper case"),
            (path.title(), "title case"),
            (path + "/", "trailing slash"),
            (path + "//", "double trailing slash"),
            (path + "/x", "extra segment"),
            (path.replace("/v1/", "//v1/"), "leading double slash"),
            (path.replace("/v1/", "/v1//"), "double slash inside"),
            (path + "%00", "NUL in path"),
            (path + "%zz", "bad escape in path"),
            (path + "%2F", "encoded slash"),
            (path.replace("ip", "%69p").replace("status", "%73tatus"), "encoded letter"),
            (path + "?", "empty query"),
            (path + "?a=1&a=2", "unrelated query"),
            (path + "?%zz=1", "bad escape in key"),
            (path + "?a=%zz", "bad escape in value"),
            (path + "?a=%00", "NUL in value"),
            (path + "?" + "&".join(f"a{i}=1" for i in range(1100)), "1100 parameters"),
            (path + "?" + "a=" + "x" * 8000, "8000 character value"),
        ):
            if "?" in variant:
                variant_path, variant_query = variant.split("?", 1)
                add(module, variant_path, variant_query, note)
            else:
                add(module, variant, None, note)
    for path in (
        "/",
        "//",
        "/v1",
        "/v1/",
        "/v1/brc20",
        "/v1/brc20/",
        "/v1/brc20//",
        "/v1/bitmap",
        "/v1/sns/",
        "/v1/runes",
        "/v1/status/",
        "/v1/status//",
        "/v1/nothing",
        "/nothing",
        "/%zz",
        "/%%41",
        "/%4",
        "/%00",
        "/%F0%9F",
        "/%E2%9C%93",
        "/v1/brc20/%E2%9C%93",
        "/v1/brc20/%3Cscript%3E",
        "/v1/brc20/a%22b",
        "/v1/brc20/a'b",
        "/v1/brc20/a%20b",
        "/.",
        "/..",
        "/v1/brc20/../status",
        "/v1/brc20/%2e%2e/status",
        "/v1/brc20/" + "a" * 5000,
    ):
        module = APP
        add(module, path, None, "routing")

    # Parameter-parsing limits of qs: depth 5, array limit 20, 1000 keys.
    add("brc20", "/v1/brc20/holders", join(f"ticker={tk}", "&".join(f"k{i}=1" for i in range(1005))), "1005 keys then ticker")
    add("brc20", "/v1/brc20/holders", join("&".join(f"k{i}=1" for i in range(995)), f"ticker={tk}"), "ticker after 995 keys")
    add("brc20", "/v1/brc20/holders", join("&".join(f"k{i}=1" for i in range(1000)), f"ticker={tk}"), "ticker after 1000 keys")
    add("brc20", "/v1/brc20/holders", f"ticker={tk}&ticker[a]=b", "string then object")
    add("brc20", "/v1/brc20/holders", f"ticker[a]=b&ticker={tk}", "object then string")
    add("brc20", "/v1/brc20/holders", f"ticker.a={tk}", "dotted key")
    add("brc20", "/v1/brc20/holders", f"%74icker={tk}", "encoded key name")
    add("brc20", "/v1/brc20/holders", f"TICKER={tk}", "upper case key name")
    add("brc20", "/v1/brc20/holders", f"ticker={tk};ticker=x", "semicolon")
    add("brc20", "/v1/brc20/holders", f"ticker={tk}&&&", "empty pairs")
    add("brc20", "/v1/brc20/holders", f"&ticker={tk}", "leading ampersand")
    add("brc20", "/v1/brc20/holders", f"=1&ticker={tk}", "empty key")
    add("brc20", "/v1/brc20/holders", "ticker=a%00b", "NUL inside")
    add("brc20", "/v1/brc20/holders", "ticker[]=" + "&ticker[]=".join([tk] * 25), "array over the limit")

    # Other methods and CORS.
    for path in (
        "/v1/brc20/ip",
        "/v1/brc20/holders",
        "/v1/bitmap/block_height",
        "/v1/sns/ip",
        "/v1/runes/ip",
        "/v1/status",
        "/v1/nothing",
        "/",
    ):
        module = path.split("/")[2] if len(path.split("/")) > 2 and path.split("/")[2] in MODULES else APP
        for method in ("HEAD", "POST", "PUT", "PATCH", "DELETE"):
            add(module, path, None, f"method {method}", method=method)
        add(module, path, None, "preflight", method="OPTIONS")
        add(
            module,
            path,
            None,
            "preflight with request headers",
            method="OPTIONS",
            headers={
                "Origin": "https://example.com",
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": "content-type,x-custom",
            },
        )
        add(
            module,
            path,
            None,
            "preflight with repeated request headers",
            method="OPTIONS",
            headers={"Access-Control-Request-Headers": "a, b"},
        )
        add(module, path, None, "GET with origin", headers={"Origin": "https://example.com"})
    add("brc20", "/v1/brc20/ip", None, "forwarded for", headers={"X-Forwarded-For": "1.2.3.4, 5.6.7.8"})
    add("brc20", "/v1/brc20/ip", None, "forwarded for, empty entries", headers={"X-Forwarded-For": ", ,1.2.3.4"})
    add("brc20", "/v1/brc20/ip", None, "accept json", headers={"Accept": "application/json"})
    add("brc20", "/v1/brc20/ip", None, "accept gzip", headers={"Accept-Encoding": "gzip, deflate, br"})
    add("brc20", "/v1/brc20/ip", None, "conditional", headers={"If-None-Match": '"x"', "If-Modified-Since": "Wed, 21 Oct 2015 07:28:00 GMT"})
    return out


def build_requests(p: dict) -> list:
    """The fixed request list: good requests first, bad ones after, the quirk
    cases, and the requests that can end an old process last. Restart the old
    apps before each run."""
    good = [
        # brc20: 19 routes
        Request("brc20", "/v1/brc20/ip"),
        Request("brc20", "/v1/brc20/db_version"),
        Request("brc20", "/v1/brc20/event_hash_version"),
        Request("brc20", "/v1/brc20/extras_block_height"),
        Request("brc20", "/v1/brc20/block_height"),
        Request(
            "brc20",
            "/v1/brc20/balance_on_block",
            {"block_height": p["height"], "pkscript": p["brc20_pkscript"], "ticker": p["ticker"]},
        ),
        Request("brc20", "/v1/brc20/activity_on_block", {"block_height": p["height"]}),
        Request("brc20", "/v1/brc20/bitcoin_rpc_results_on_block", {"block_height": p["height"]}),
        Request(
            "brc20",
            "/v1/brc20/get_current_balance_of_wallet",
            {"pkscript": p["brc20_pkscript"], "ticker": p["ticker"]},
        ),
        Request(
            "brc20",
            "/v1/brc20/get_valid_tx_notes_of_wallet",
            {"pkscript": p["brc20_pkscript"]},
        ),
        Request("brc20", "/v1/brc20/get_valid_tx_notes_of_ticker", {"ticker": p["ticker"]}),
        Request("brc20", "/v1/brc20/holders", {"ticker": p["ticker"]}),
        Request("brc20", "/v1/brc20/get_hash_of_all_activity", {"block_height": p["height"]}),
        Request("brc20", "/v1/brc20/get_hash_of_all_current_balances"),
        Request("brc20", "/v1/brc20/event", {"inscription_id": p["inscription_id"]}),
        Request("brc20", f"/v1/brc20/explorer/btc_address/{p['evm_address']}"),
        Request("brc20", f"/v1/brc20/explorer/balances/{p['evm_address']}"),
        Request("brc20", f"/v1/brc20/explorer/address_tabs_counters/{p['evm_address']}"),
        Request("brc20", "/v1/brc20/explorer/tokens", {"q": "ord", "items_count": "0", "order": "asc"}),
        # bitmap: 5 routes
        Request("bitmap", "/v1/bitmap/ip"),
        Request("bitmap", "/v1/bitmap/block_height"),
        Request("bitmap", "/v1/bitmap/get_hash_of_all_activity", {"block_height": p["height"]}, crash_prone=True),
        Request("bitmap", "/v1/bitmap/get_hash_of_all_bitmaps", crash_prone=True),
        Request(
            "bitmap",
            "/v1/bitmap/get_inscription_id_of_bitmap",
            {"bitmap_number": p["bitmap_number"]},
        ),
        # sns: 7 routes, get_registered_namespaces is appended at the very end
        Request("sns", "/v1/sns/ip"),
        Request("sns", "/v1/sns/block_height"),
        Request("sns", "/v1/sns/get_hash_of_all_activity", {"block_height": p["height"]}, crash_prone=True),
        Request("sns", "/v1/sns/get_hash_of_all_registered_names"),
        Request("sns", "/v1/sns/get_info_of_sns", {"name": p["sns_name"]}),
        Request("sns", "/v1/sns/get_inscriptions_of_domain", {"domain": p["sns_domain"]}),
        # runes: 9 routes
        Request("runes", "/v1/runes/ip"),
        Request("runes", "/v1/runes/block_height"),
        Request(
            "runes",
            "/v1/runes/balance_on_block",
            {"block_height": p["height"], "pkscript": p["pkscript"]},
        ),
        Request("runes", "/v1/runes/activity_on_block", {"block_height": p["height"]}),
        Request("runes", "/v1/runes/get_current_balance_of_wallet", {"pkscript": p["pkscript"]}),
        Request("runes", "/v1/runes/get_current_balance_of_wallet", {"address": p["address"]}),
        Request(
            "runes",
            "/v1/runes/get_unspent_rune_outpoints_of_wallet",
            {"pkscript": p["pkscript"]},
        ),
        Request("runes", "/v1/runes/holders", {"rune_name": p["rune_name"]}),
        Request("runes", "/v1/runes/holders", {"rune_id": p["rune_id"], "rune_name": ""}),
        Request("runes", "/v1/runes/get_hash_of_all_activity", {"block_height": p["height"]}),
        Request("runes", "/v1/runes/event", {"transaction_id": p["txid"]}),
        # app: the status route, answered by the merged app only
        Request(APP, "/v1/status"),
    ]
    bad = [
        Request("brc20", "/v1/brc20/balance_on_block", None, "bad", "no params"),
        Request("brc20", "/v1/brc20/balance_on_block", {"block_height": "abc", "pkscript": "00", "ticker": "x"}, "bad", "height not a number"),
        Request("brc20", "/v1/brc20/activity_on_block", None, "bad", "no params"),
        Request("brc20", "/v1/brc20/activity_on_block", {"block_height": "-1"}, "bad", "negative height"),
        Request("brc20", "/v1/brc20/get_current_balance_of_wallet", {"pkscript": "00"}, "bad", "no ticker"),
        Request("brc20", "/v1/brc20/get_valid_tx_notes_of_wallet", None, "bad", "no wallet"),
        Request("brc20", "/v1/brc20/holders", None, "bad", "no ticker"),
        Request("brc20", "/v1/brc20/event", None, "bad", "no inscription id"),
        Request("brc20", "/v1/brc20/event", {"inscription_id": "zzz"}, "bad", "unknown inscription"),
        Request("brc20", "/v1/brc20/explorer/balances/notanaddress", None, "bad", "malformed address"),
        Request("brc20", "/v1/brc20/explorer/tokens", {"items_count": "abc", "order": "up"}, "bad", "junk paging"),
        Request("brc20", "/v1/brc20/no_such_route", None, "bad", "unknown route"),
        Request("bitmap", "/v1/bitmap/get_inscription_id_of_bitmap", None, "bad", "no number"),
        Request("bitmap", "/v1/bitmap/get_inscription_id_of_bitmap", {"bitmap_number": "abc"}, "bad", "number not numeric", crash_prone=True),
        Request("bitmap", "/v1/bitmap/no_such_route", None, "bad", "unknown route"),
        Request("sns", "/v1/sns/get_info_of_sns", None, "bad", "no name"),
        Request("sns", "/v1/sns/get_inscriptions_of_domain", None, "bad", "no domain"),
        Request("sns", "/v1/sns/no_such_route", None, "bad", "unknown route"),
        Request("runes", "/v1/runes/balance_on_block", None, "bad", "no params"),
        Request("runes", "/v1/runes/activity_on_block", {"block_height": "abc"}, "bad", "height not a number"),
        Request("runes", "/v1/runes/holders", None, "bad", "no rune name"),
        Request("runes", "/v1/runes/event", None, "bad", "no txid"),
        Request("runes", "/v1/runes/no_such_route", None, "bad", "unknown route"),
    ]
    everything = good + bad + build_quirk_requests(p)
    safe = [r for r in everything if not r.crash_prone]
    prone = [r for r in everything if r.crash_prone]
    return safe + prone + [Request("sns", "/v1/sns/get_registered_namespaces", crash_prone=True)]


def rate_limit_requests() -> list:
    """The sequence replayed by `--rate-limit`: counted requests of several
    kinds interleaved with preflights, which the limiter does not count."""
    seq = []
    for i in range(6):
        seq.append(Request(APP, "/v1/brc20/ip", note=f"counted {i + 1}"))
        if i == 1:
            seq.append(Request(APP, "/v1/brc20/ip", method="OPTIONS", note="preflight"))
        if i == 2:
            seq.append(Request(APP, "/v1/status", note="status"))
        if i == 3:
            seq.append(Request(APP, "/v1/nothing", note="unknown path"))
    seq.append(Request(APP, "/v1/brc20/ip", method="HEAD", note="head"))
    seq.append(Request(APP, "/v1/brc20/ip", method="OPTIONS", note="preflight when limited"))
    return seq


def fetch(base: str, request: Request, timeout: float) -> Response:
    url = base.rstrip("/") + request.target()
    req = urllib.request.Request(
        url,
        method=request.method,
        headers=request.headers or {},
        data=b"" if request.method in ("POST", "PUT", "PATCH") else None,
    )

    def collect(reply, status):
        headers = {}
        for name, value in reply.headers.items():
            name = name.lower()
            headers[name] = f"{headers[name]}, {value}" if name in headers else value
        raw = reply.read().decode("utf-8", "replace")
        return parse_response(status, raw, headers.get("content-type", ""), headers)

    try:
        with urllib.request.urlopen(req, timeout=timeout) as reply:
            return collect(reply, reply.status)
    except urllib.error.HTTPError as err:
        return collect(err, err.code)
    except (urllib.error.URLError, OSError) as err:
        return Response(status=0, body=str(err), raw=f"connection error: {err}")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--pair", choices=("old-new", "js-rust"), default="old-new")
    for module in MODULES:
        parser.add_argument(f"--{module}", default=DEFAULT_BASES[module], help=f"old {module} API base URL (old-new)")
    parser.add_argument("--new", default=DEFAULT_NEW, help="merged API base URL (old-new)")
    parser.add_argument("--js", default=DEFAULT_JS, help="Express API base URL (js-rust)")
    parser.add_argument("--rust", default=DEFAULT_RUST, help="Rust API base URL (js-rust)")
    parser.add_argument("--only", action="append", choices=MODULES, help="restrict to a module (repeatable)")
    parser.add_argument("--rate-limit", action="store_true", help="replay the rate-limit sequence on fresh servers (js-rust)")
    parser.add_argument("--timeout", type=float, default=60.0)
    for name, (default, help_text) in PARAMS.items():
        parser.add_argument(f"--{name.replace('_', '-')}", default=default, help=f"{help_text} (default: %(default)s)")
    return parser.parse_args(argv)


def module_counts(requests: list) -> str:
    counts = {}
    for r in requests:
        counts[r.module] = counts.get(r.module, 0) + 1
    return ", ".join(f"{m} {counts[m]}" for m in (*MODULES, APP) if m in counts)


def run_old_new(args, params) -> int:
    old_bases = {module: getattr(args, module) for module in MODULES}
    requests = [
        r
        for r in build_requests(params)
        if r.kind != "quirk"
        and r.module != APP
        and (not args.only or r.module in args.only)
    ]

    if not requests:
        print("no requests to run (check --only)", file=sys.stderr)
        return 2

    counts = {"PASS": 0, "ALLOWED": 0, "DIFF": 0}
    for request in requests:
        old = fetch(old_bases[request.module], request, args.timeout)
        new = fetch(args.new, request, args.timeout)
        verdict, detail = classify(request, old, new)
        counts[verdict] += 1
        line = f"{verdict:<7} {request.module:<6} {request.kind:<5} {old.status}/{new.status} {request.label()}"
        print(line)
        if detail:
            print(f"        {detail}")
    print()
    print(
        f"{len(requests)} requests: {counts['PASS']} PASS, "
        f"{counts['ALLOWED']} ALLOWED (documented differences), {counts['DIFF']} DIFF"
    )
    return 1 if counts["DIFF"] else 0


def run_js_rust(args, params) -> int:
    requests = [
        r
        for r in build_requests(params)
        if not args.only or r.module in args.only or r.module == APP
    ]
    if not requests:
        print("no requests to run (check --only)", file=sys.stderr)
        return 2

    counts = {"PASS": 0, "ORDER": 0, "DIFF": 0}
    order_routes = {}
    for request in requests:
        js = fetch(args.js, request, args.timeout)
        rust = fetch(args.rust, request, args.timeout)
        verdict, detail = classify_pair(request, js, rust)
        counts[verdict] += 1
        if verdict == "ORDER":
            order_routes[request.route] = order_routes.get(request.route, 0) + 1
        if verdict != "PASS" or request.kind != "quirk":
            print(f"{verdict:<5} {request.module:<6} {request.kind:<5} {js.status}/{rust.status} {request.label()}")
            if detail:
                print(f"      {detail}")
    print()
    print(f"requests per module: {module_counts(requests)}")
    for route, count in sorted(order_routes.items()):
        print(f"ORDER route {route}: {count}")
    print(
        f"{len(requests)} requests: {counts['PASS']} PASS, "
        f"{counts['ORDER']} ORDER (row order only), {counts['DIFF']} DIFF"
    )
    return 1 if counts["DIFF"] else 0


def rate_limit_headers_match(a: Response, b: Response) -> bool:
    """RateLimit-Reset counts down in whole seconds from each server's own
    first hit, so it may differ by one second between the two servers."""
    for name in set(a.headers) | set(b.headers):
        if name in VOLATILE_HEADERS:
            continue
        x, y = a.headers.get(name), b.headers.get(name)
        if x == y:
            continue
        if name in ("ratelimit-reset", "retry-after") and x and y and x.isdigit() and y.isdigit() and abs(int(x) - int(y)) <= 1:
            continue
        return False
    return True


def run_rate_limit(args) -> int:
    requests = rate_limit_requests()
    js = [fetch(args.js, r, args.timeout) for r in requests]
    rust = [fetch(args.rust, r, args.timeout) for r in requests]
    differences = 0
    for request, a, b in zip(requests, js, rust):
        same = (
            a.status == b.status
            and a.content_type == b.content_type
            and a.raw == b.raw
            and rate_limit_headers_match(a, b)
        )
        verdict = "PASS" if same else "DIFF"
        differences += 0 if same else 1
        limit = {k: v for k, v in a.headers.items() if k.startswith("ratelimit") or k == "retry-after"}
        print(f"{verdict:<5} {a.status}/{b.status} {request.label()} {limit}")
        if not same:
            print(f"      js={a.headers} {clip(a.raw)}")
            print(f"      rust={b.headers} {clip(b.raw)}")
    print()
    print(f"{len(requests)} requests: {len(requests) - differences} PASS, {differences} DIFF")
    return 1 if differences else 0


def run(args) -> int:
    params = {name: getattr(args, name) for name in PARAMS}
    if args.pair == "js-rust":
        if args.rate_limit:
            return run_rate_limit(args)
        return run_js_rust(args, params)
    return run_old_new(args, params)


def main(argv=None) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
