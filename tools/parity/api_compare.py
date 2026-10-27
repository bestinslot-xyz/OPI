#!/usr/bin/env python3
"""Replays one request list against the four per-module APIs and the merged API
and diffs status code and body.

Every route of every module is requested with realistic parameters, then with
bad parameters. A request passes when old and new answer with the same status
and the same body. Differences that are documented behaviour changes sit in
ALLOWED and are reported separately; any other difference makes the exit code 1.

    python3 tools/parity/api_compare.py \\
        --brc20 http://127.0.0.1:8000 --bitmap http://127.0.0.1:8001 \\
        --sns http://127.0.0.1:8002 --runes http://127.0.0.1:8003 \\
        --new http://127.0.0.1:8010

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
from dataclasses import dataclass
from typing import Any, Callable, Optional
from urllib.parse import urlencode

MODULES = ("brc20", "bitmap", "sns", "runes")

DEFAULT_BASES = {
    "brc20": "http://127.0.0.1:8000",
    "bitmap": "http://127.0.0.1:8001",
    "sns": "http://127.0.0.1:8002",
    "runes": "http://127.0.0.1:8003",
}
DEFAULT_NEW = "http://127.0.0.1:8010"

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
    """One replayed request. `path` is the same on the old and the new server."""

    module: str
    path: str
    query: Optional[dict] = None
    kind: str = "good"  # "good" or "bad"
    note: str = ""
    # The old handler throws outside any error handling, which ends the old
    # process. Such requests run after all others.
    crash_prone: bool = False

    @property
    def route(self) -> str:
        """The route the request belongs to, with path parameters left in place."""
        parts = self.path.split("/")
        if len(parts) > 4 and parts[3] == "explorer":
            return "/".join(parts[:5])
        return self.path

    def target(self) -> str:
        if self.query:
            return self.path + "?" + urlencode(self.query)
        return self.path

    def label(self) -> str:
        text = self.target()
        if self.note:
            text += f"  [{self.note}]"
        return text


@dataclass
class Response:
    """A server answer. Status 0 means no HTTP answer (connection error)."""

    status: int
    body: Any
    raw: str = ""


def parse_response(status: int, raw: str) -> Response:
    """Builds a Response, decoding the body as JSON when it parses."""
    try:
        body = json.loads(raw)
    except ValueError:
        body = raw
    return Response(status=status, body=body, raw=raw)


def diff_responses(old: Response, new: Response) -> Optional[str]:
    """Returns None when both answers match, else a one-line description."""
    if old.status != new.status:
        return f"status old={old.status} new={new.status}"
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
        "R5: the old handler had no error handling and crashed the process; "
        "the merged API answers",
        _old_unreachable_or_failed,
    ),
]
# Routes whose old handler throws outside any error handling on some inputs
# (an async rejection ends the old process); the merged API answers 500.
for _route in (
    "/v1/bitmap/get_hash_of_all_activity",
    "/v1/bitmap/get_hash_of_all_bitmaps",
    "/v1/bitmap/get_inscription_id_of_bitmap",
    "/v1/sns/get_hash_of_all_activity",
):
    ALLOWED.append(
        AllowRule(
            _route,
            "R5: the old handler had no error handling and an unhandled rejection "
            "ended the process; the merged API answers 500",
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


def build_requests(p: dict) -> list:
    """The fixed request list: good requests first, bad ones after, and the
    requests that can end an old process last. Restart the old apps before each run."""
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
    everything = good + bad
    safe = [r for r in everything if not r.crash_prone]
    prone = [r for r in everything if r.crash_prone]
    return safe + prone + [Request("sns", "/v1/sns/get_registered_namespaces", crash_prone=True)]


def fetch(base: str, request: Request, timeout: float) -> Response:
    url = base.rstrip("/") + request.target()
    try:
        with urllib.request.urlopen(url, timeout=timeout) as reply:
            return parse_response(reply.status, reply.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as err:
        return parse_response(err.code, err.read().decode("utf-8", "replace"))
    except (urllib.error.URLError, OSError) as err:
        return Response(status=0, body=str(err), raw=f"connection error: {err}")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    for module in MODULES:
        parser.add_argument(f"--{module}", default=DEFAULT_BASES[module], help=f"old {module} API base URL")
    parser.add_argument("--new", default=DEFAULT_NEW, help="merged API base URL")
    parser.add_argument("--only", action="append", choices=MODULES, help="restrict to a module (repeatable)")
    parser.add_argument("--timeout", type=float, default=60.0)
    for name, (default, help_text) in PARAMS.items():
        parser.add_argument(f"--{name.replace('_', '-')}", default=default, help=f"{help_text} (default: %(default)s)")
    return parser.parse_args(argv)


def run(args) -> int:
    params = {name: getattr(args, name) for name in PARAMS}
    old_bases = {module: getattr(args, module) for module in MODULES}
    requests = [r for r in build_requests(params) if not args.only or r.module in args.only]

    counts = {"PASS": 0, "ALLOWED": 0, "DIFF": 0}
    for request in requests:
        old = fetch(old_bases[request.module], request, args.timeout)
        new = fetch(args.new, request, args.timeout)
        verdict, detail = classify(request, old, new)
        counts[verdict] += 1
        line = f"{verdict:<7} {request.module:<6} {request.kind:<4} {old.status}/{new.status} {request.label()}"
        print(line)
        if detail:
            print(f"        {detail}")
    print()
    print(
        f"{len(requests)} requests: {counts['PASS']} PASS, "
        f"{counts['ALLOWED']} ALLOWED (documented differences), {counts['DIFF']} DIFF"
    )
    return 1 if counts["DIFF"] else 0


def main(argv=None) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
