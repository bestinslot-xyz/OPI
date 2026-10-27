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
