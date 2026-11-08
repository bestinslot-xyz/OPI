"""Tests for the pure parts of api_compare.py: response parsing, diffing, the
allow-list and the request list. They need no network and no database."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import api_compare as ac  # noqa: E402


def resp(status, raw):
    return ac.parse_response(status, raw)


def test_parse_response_decodes_json_and_keeps_text():
    assert resp(200, '{"a": 1}').body == {"a": 1}
    assert resp(200, "::1").body == "::1"
    assert resp(200, "328112").body == 328112


def test_diff_equal_answers_match():
    assert ac.diff_responses(resp(200, '{"a":1,"b":2}'), resp(200, '{"b": 2, "a": 1}')) is None


def test_diff_reports_status_before_body():
    msg = ac.diff_responses(resp(200, "{}"), resp(500, "{}"))
    assert msg == "status old=200 new=500"


def test_diff_reports_body():
    msg = ac.diff_responses(resp(200, '{"a":1}'), resp(200, '{"a":2}'))
    assert msg.startswith("body old=")


def test_connection_error_is_status_zero():
    assert ac.diff_responses(ac.Response(0, "x", "x"), resp(200, "{}")) is not None


def test_clip_shortens_long_text():
    assert len(ac.clip("x" * 500)) < 200
    assert ac.clip("a   b\n c") == "a b c"


def request(path, **kw):
    return ac.Request("m", path, **kw)


def test_route_keeps_explorer_path_parameter_out():
    assert request("/v1/brc20/explorer/balances/0xabc").route == "/v1/brc20/explorer/balances"
    assert request("/v1/brc20/holders", query={"ticker": "x"}).route == "/v1/brc20/holders"


def test_target_encodes_query():
    assert request("/v1/x", query={"a": "b c"}).target() == "/v1/x?a=b+c"
    assert request("/v1/x").target() == "/v1/x"


def test_equal_answers_pass_even_on_allowed_route():
    req = request("/v1/bitmap/block_height")
    assert ac.classify(req, resp(200, "{}"), resp(200, "{}")) == ("PASS", "")


def test_r4_block_height_is_allowed_only_for_the_documented_shape():
    req = request("/v1/bitmap/block_height")
    verdict, detail = ac.classify(req, resp(200, "{}"), resp(200, "328112"))
    assert verdict == "ALLOWED" and "R4" in detail
    verdict, _ = ac.classify(req, resp(200, '{"x":1}'), resp(200, "328112"))
    assert verdict == "DIFF"
    verdict, _ = ac.classify(req, resp(200, "{}"), resp(500, "{}"))
    assert verdict == "DIFF"


def test_r4_applies_to_sns_and_not_to_other_modules():
    assert ac.classify(request("/v1/sns/block_height"), resp(200, "{}"), resp(200, "5"))[0] == "ALLOWED"
    assert ac.classify(request("/v1/runes/block_height"), resp(200, "{}"), resp(200, "5"))[0] == "DIFF"


def test_r5_registered_namespaces_old_failure_is_allowed():
    req = request("/v1/sns/get_registered_namespaces")
    new = resp(200, '{"error":null,"result":[]}')
    assert ac.classify(req, ac.Response(0, "refused", "refused"), new)[0] == "ALLOWED"
    assert ac.classify(req, resp(500, "boom"), new)[0] == "ALLOWED"
    assert ac.classify(req, resp(200, '{"result":[1]}'), new)[0] == "DIFF"


def test_ip_difference_is_allowed():
    req = request("/v1/brc20/ip")
    assert ac.classify(req, resp(200, "::1"), resp(200, "::ffff:127.0.0.1"))[0] == "ALLOWED"
    assert ac.classify(req, resp(200, "::1"), resp(500, "x"))[0] == "DIFF"


def test_custom_rules_are_honoured():
    rule = ac.AllowRule("/v1/z", "because", lambda old, new: True)
    verdict, detail = ac.classify(request("/v1/z"), resp(200, "1"), resp(200, "2"), [rule])
    assert verdict == "ALLOWED" and detail.startswith("because")
    assert ac.classify(request("/v1/z"), resp(200, "1"), resp(200, "2"), [])[0] == "DIFF"


def defaults():
    return {name: value for name, (value, _) in ac.PARAMS.items()}


def test_request_list_covers_every_route():
    routes = {(r.module, r.route) for r in ac.build_requests(defaults()) if r.kind == "good"}
    counts = {m: len({route for mod, route in routes if mod == m}) for m in ac.MODULES}
    assert counts == {"brc20": 19, "bitmap": 5, "sns": 7, "runes": 9}


def test_request_list_has_bad_cases_and_ends_with_the_crashing_route():
    requests = ac.build_requests(defaults())
    assert any(r.kind == "bad" for r in requests)
    assert requests[-1].path == "/v1/sns/get_registered_namespaces"


def test_crash_prone_requests_run_after_the_others():
    requests = ac.build_requests(defaults())
    flags = [r.crash_prone for r in requests]
    first = flags.index(True)
    assert all(flags[first:]) and not any(flags[:first])


def test_old_process_crash_on_a_handler_error_is_allowed():
    req = request("/v1/bitmap/get_hash_of_all_bitmaps")
    assert ac.classify(req, ac.Response(0, "x", "x"), resp(500, "{}"))[0] == "ALLOWED"
    assert ac.classify(req, ac.Response(0, "x", "x"), resp(200, "{}"))[0] == "DIFF"
    assert ac.classify(req, resp(200, "{}"), resp(500, "{}"))[0] == "DIFF"


def test_r6_hash_of_all_activity_old_crash_is_allowed_when_new_answers_200():
    for path in ("/v1/bitmap/get_hash_of_all_activity", "/v1/sns/get_hash_of_all_activity"):
        req = request(path)
        verdict, detail = ac.classify(req, ac.Response(0, "x", "x"), resp(200, "{}"))
        assert verdict == "ALLOWED" and "R6" in detail
        assert ac.classify(req, resp(500, "x"), resp(200, "{}"))[0] == "ALLOWED"
        assert ac.classify(req, ac.Response(0, "x", "x"), resp(500, "{}"))[0] == "DIFF"
        assert ac.classify(req, resp(200, "{}"), resp(500, "{}"))[0] == "DIFF"


def test_an_empty_request_list_exits_2(monkeypatch, capsys):
    monkeypatch.setattr(ac, "build_requests", lambda params: [])
    assert ac.main(["--only", "bitmap"]) == 2
    assert "no requests" in capsys.readouterr().err


def test_args_override_defaults_and_exit_code_follows_diffs(monkeypatch, capsys):
    args = ac.parse_args(["--ticker", "abcd", "--only", "bitmap"])
    assert args.ticker == "abcd" and args.only == ["bitmap"]

    answers = {}

    def fake_fetch(base, request, timeout):
        return answers.get(base, resp(200, "{}"))

    monkeypatch.setattr(ac, "fetch", fake_fetch)
    assert ac.run(args) == 0
    answers[args.new] = resp(200, '{"changed": true}')
    assert ac.run(args) == 1
    assert "DIFF" in capsys.readouterr().out


def full(status, raw, content_type="application/json; charset=utf-8", **headers):
    return ac.parse_response(status, raw, content_type, headers)


def test_content_type_is_compared_in_both_modes():
    msg = ac.diff_responses(full(200, "x", "text/html"), full(200, "x", "application/json"))
    assert msg.startswith("content-type")
    verdict, detail = ac.classify_pair(request("/v1/brc20/ip"), full(200, "x", "text/html"), full(200, "x", "application/json"))
    assert verdict == "DIFF" and detail.startswith("content-type")


def test_pair_compares_raw_bodies_so_key_order_counts():
    req = request("/v1/brc20/holders")
    assert ac.classify_pair(req, full(200, '{"a":1,"b":2}'), full(200, '{"b":2,"a":1}'))[0] == "DIFF"
    rows_a = full(200, '{"r":[{"a":1,"b":2}]}')
    rows_b = full(200, '{"r":[{"b":2,"a":1}]}')
    assert ac.classify_pair(req, rows_a, rows_b)[0] == "DIFF"
    assert ac.classify_pair(req, full(200, '{"a":1}'), full(200, '{"a":1}'))[0] == "PASS"


def test_pair_compares_headers_except_the_volatile_ones():
    req = request("/v1/brc20/ip")
    same = ac.classify_pair(req, full(200, "x", date="a", connection="close"), full(200, "x", date="b"))
    assert same[0] == "PASS"
    other = ac.classify_pair(req, full(200, "x", vary="a"), full(200, "x"))
    assert other[0] == "DIFF" and "vary" in other[1]


def test_pair_has_no_allowed_entries():
    for path in ("/v1/bitmap/block_height", "/v1/sns/get_registered_namespaces", "/v1/brc20/ip"):
        assert ac.classify_pair(request(path), full(200, "{}"), full(200, "328112"))[0] == "DIFF"
        assert ac.classify_pair(request(path), ac.Response(0, "x", "x"), full(200, "{}"))[0] == "DIFF"


def test_modules_key_order_is_only_free_on_the_status_route():
    a = full(200, '{"error":null,"result":{"modules":{"brc20":1,"sns":2}}}')
    b = full(200, '{"error":null,"result":{"modules":{"sns":2,"brc20":1}}}')
    assert ac.classify_pair(request("/v1/status"), a, b)[0] == "PASS"
    assert ac.classify_pair(request("/v1/brc20/activity_on_block"), a, b)[0] == "DIFF"


def test_status_modules_nested_in_result_compare_without_key_order():
    req = request("/v1/status")
    a = full(200, '{"error":null,"result":{"modules":{"brc20":{"h":1},"sns":{"h":2}}}}')
    b = full(200, '{"error":null,"result":{"modules":{"sns":{"h":2},"brc20":{"h":1}}}}')
    assert ac.classify_pair(req, a, b)[0] == "PASS"
    c = full(200, '{"error":null,"result":{"modules":{"sns":{"h":3},"brc20":{"h":1}}}}')
    assert ac.classify_pair(req, a, c)[0] == "DIFF"
    d = full(200, '{"result":{"modules":{"sns":{"h":2},"brc20":{"h":1}}},"error":null}')
    assert ac.classify_pair(req, a, d)[0] == "DIFF"


def test_row_order_on_an_unordered_route_is_order_not_diff():
    req = request("/v1/runes/holders")
    a = full(200, '{"error":null,"result":[{"w":"a","n":1},{"w":"b","n":2}]}')
    b = full(200, '{"error":null,"result":[{"w":"b","n":2},{"w":"a","n":1}]}')
    assert ac.classify_pair(req, a, b)[0] == "ORDER"
    other = full(200, '{"error":null,"result":[{"w":"b","n":2},{"w":"a","n":9}]}')
    assert ac.classify_pair(req, a, other)[0] == "DIFF"
    short = full(200, '{"error":null,"result":[{"w":"a","n":1}]}')
    assert ac.classify_pair(req, a, short)[0] == "DIFF"


def test_row_order_on_an_ordered_route_is_a_diff():
    a = full(200, '{"result":[{"w":"a"},{"w":"b"}]}')
    b = full(200, '{"result":[{"w":"b"},{"w":"a"}]}')
    assert ac.classify_pair(request("/v1/brc20/activity_on_block"), a, b)[0] == "DIFF"


def test_scalar_lists_keep_their_order_even_on_unordered_routes():
    req = request("/v1/runes/get_unspent_rune_outpoints_of_wallet")
    a = full(200, '{"result":[{"rune_ids":["a","b"],"balances":[1,2]}]}')
    b = full(200, '{"result":[{"rune_ids":["b","a"],"balances":[2,1]}]}')
    assert ac.classify_pair(req, a, b)[0] == "DIFF"


def test_unordered_routes_are_the_documented_ones():
    assert ac.UNORDERED_ROUTES == {
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


def test_route_ignores_case_and_a_trailing_slash():
    assert request("/V1/RUNES/Holders/").route == "/v1/runes/holders"
    assert request("/v1/BRC20/explorer/Balances/0xAB").route == "/v1/brc20/explorer/balances"


def test_raw_query_is_sent_as_is():
    assert request("/v1/x", raw_query="a=1&a=2&b[]=%zz").target() == "/v1/x?a=1&a=2&b[]=%zz"
    assert request("/v1/x", raw_query="").target() == "/v1/x?"


def quirk_requests():
    return [r for r in ac.build_requests(defaults()) if r.kind == "quirk"]


def test_quirk_cases_cover_every_documented_quirk():
    targets = [r.target() for r in quirk_requests()]

    def has(*needles):
        return any(all(n in t for n in needles) for t in targets)

    assert has("block_height=1&block_height=2")  # repeated
    assert has("block_height[]=1")  # array
    assert has("block_height[a]=1")  # object
    assert has("block_height[toString]=x")  # own toString
    assert has("block_height=NaN") and has("block_height=Infinity")
    assert has("block_height=0x10") and has("block_height=1_0")
    assert has("block_height=%00") and has("block_height=%zz")
    assert has("block_height=" + "9" * 5000)  # very long
    assert has("ticker[]=1") and has("ticker[a]=1") and has("ticker=1&ticker=2")
    assert has("address[]=") and has("rune_id[]=")
    assert has("items_count=1e21") and has("items_count=-0")
    assert has("order=ASC") and has("order[]=1") and has("order=asc&order=desc")
    assert has("/explorer/balances/%zz") and has("/explorer/btc_address/%00")
    assert has("%C4%B0")  # toLowerCase changes the length
    assert has("/v1/status/x")


def test_quirk_cases_include_other_methods_and_preflights():
    methods = {r.method for r in quirk_requests()}
    assert {"HEAD", "POST", "OPTIONS", "PUT", "PATCH", "DELETE"} <= methods
    assert any(r.headers and "Access-Control-Request-Headers" in r.headers for r in quirk_requests())


def test_old_new_mode_skips_quirk_and_app_requests(monkeypatch, capsys):
    seen = []

    def fake_fetch(base, request, timeout):
        seen.append(request)
        return resp(200, "{}")

    monkeypatch.setattr(ac, "fetch", fake_fetch)
    assert ac.run(ac.parse_args([])) == 0
    assert seen and all(r.kind != "quirk" and r.module != ac.APP for r in seen)


def test_js_rust_mode_runs_everything_against_two_servers(monkeypatch, capsys):
    bases = []

    def fake_fetch(base, request, timeout):
        bases.append(base)
        return full(200, "{}")

    monkeypatch.setattr(ac, "fetch", fake_fetch)
    args = ac.parse_args(["--pair", "js-rust", "--js", "http://js", "--rust", "http://rust"])
    assert ac.run(args) == 0
    assert set(bases) == {"http://js", "http://rust"}
    out = capsys.readouterr().out
    assert "requests per module: brc20" in out and "0 DIFF" in out


def test_js_rust_mode_fails_on_any_difference(monkeypatch, capsys):
    def fake_fetch(base, request, timeout):
        return full(200, "{}" if base == "http://js" else "[]")

    monkeypatch.setattr(ac, "fetch", fake_fetch)
    args = ac.parse_args(["--pair", "js-rust", "--js", "http://js", "--rust", "http://rust", "--only", "bitmap"])
    assert ac.run(args) == 1


def test_rate_limit_reset_may_differ_by_one_second_only():
    a = full(429, "x", **{"ratelimit-reset": "10", "ratelimit-limit": "3"})
    assert ac.rate_limit_headers_match(a, full(429, "x", **{"ratelimit-reset": "9", "ratelimit-limit": "3"}))
    assert not ac.rate_limit_headers_match(a, full(429, "x", **{"ratelimit-reset": "8", "ratelimit-limit": "3"}))
    assert not ac.rate_limit_headers_match(a, full(429, "x", **{"ratelimit-reset": "10", "ratelimit-limit": "4"}))


def test_rate_limit_run_compares_position_by_position(monkeypatch, capsys):
    def fake_fetch(base, request, timeout):
        return full(200, "ok", **{"ratelimit-limit": "3"})

    monkeypatch.setattr(ac, "fetch", fake_fetch)
    args = ac.parse_args(["--pair", "js-rust", "--rate-limit"])
    assert ac.run(args) == 0

    calls = {"n": 0}

    def drifting(base, request, timeout):
        calls["n"] += 1
        return full(200 if base == args.js else 429, "ok")

    monkeypatch.setattr(ac, "fetch", drifting)
    assert ac.run(args) == 1


def unreachable():
    return ac.Response(0, "connection refused", "connection error: connection refused")


def test_two_unreachable_servers_are_a_diff():
    verdict, detail = ac.classify_pair(request("/v1/brc20/ip"), unreachable(), unreachable())
    assert verdict == "DIFF" and "no HTTP answer" in detail
    assert ac.classify_pair(request("/v1/brc20/ip"), unreachable(), full(200, "x"))[0] == "DIFF"
    assert ac.classify_pair(request("/v1/brc20/ip"), full(200, "x"), unreachable())[0] == "DIFF"


def test_unreachable_servers_fail_the_runs(monkeypatch, capsys):
    monkeypatch.setattr(ac, "fetch", lambda base, request, timeout: unreachable())
    assert ac.run(ac.parse_args(["--pair", "js-rust", "--only", "bitmap"])) == 1
    assert ac.run(ac.parse_args(["--pair", "js-rust", "--rate-limit"])) == 1


def test_status_module_values_compare_by_exact_json_text():
    req = request("/v1/status")

    def status(modules):
        return full(200, '{"error":null,"result":{"modules":' + modules + "}}")

    base = status('{"brc20":{"a":1,"b":2},"sns":{"h":1}}')
    assert ac.classify_pair(req, base, status('{"sns":{"h":1},"brc20":{"a":1,"b":2}}'))[0] == "PASS"
    assert ac.classify_pair(req, base, status('{"brc20":{"b":2,"a":1},"sns":{"h":1}}'))[0] == "DIFF"
    for spelling in ("1.0", "true"):
        other = status('{"brc20":{"a":1,"b":2},"sns":{"h":' + spelling + "}}")
        assert ac.classify_pair(req, base, other)[0] == "DIFF"
    assert ac.classify_pair(req, base, status('{"brc20":{"a":1,"b":2}}'))[0] == "DIFF"


def test_only_the_row_list_is_reordered_not_the_contents_of_a_row():
    req = request("/v1/runes/holders")
    a = full(200, '{"error":null,"result":[{"id":1,"xs":[{"k":1},{"k":2}]},{"id":2,"xs":[]}]}')
    rows_moved = full(200, '{"error":null,"result":[{"id":2,"xs":[]},{"id":1,"xs":[{"k":1},{"k":2}]}]}')
    assert ac.classify_pair(req, a, rows_moved)[0] == "ORDER"
    inner_moved = full(200, '{"error":null,"result":[{"id":1,"xs":[{"k":2},{"k":1}]},{"id":2,"xs":[]}]}')
    assert ac.classify_pair(req, a, inner_moved)[0] == "DIFF"
    nested = full(200, '{"result":{"items":[{"a":1},{"a":2}]}}')
    nested_moved = full(200, '{"result":{"items":[{"a":2},{"a":1}]}}')
    assert ac.classify_pair(request("/v1/brc20/explorer/balances"), nested, nested_moved)[0] == "ORDER"
