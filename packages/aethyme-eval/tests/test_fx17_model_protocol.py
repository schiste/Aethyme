"""No-model regressions for the FX17 provider schema and event protocol."""

from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path

import pytest

_RUNNER_PATH = (Path(__file__).resolve().parents[1] / "evals" / "fx17" / "run_fx17.py")
_SPEC = importlib.util.spec_from_file_location("fx17_run_fx17_model_protocol_test", _RUNNER_PATH)
assert _SPEC is not None and _SPEC.loader is not None
fx17 = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fx17)


def test_response_schema_uses_strict_supported_root_object() -> None:
    fx17.validate_response_schema()
    schema = fx17.RESPONSE_SCHEMA
    assert schema["type"] == "object"
    assert schema["additionalProperties"] is False
    assert schema["required"] == ["app_sh", "refusal"]
    assert set(schema["properties"]) == {"app_sh", "refusal"}
    assert all(prop == {"type": ["string", "null"]}
               for prop in schema["properties"].values())
    assert not {"oneOf", "anyOf", "allOf", "enum", "not"}.intersection(schema)


def test_response_schema_gate_rejects_root_composition() -> None:
    schema = copy.deepcopy(fx17.RESPONSE_SCHEMA)
    schema["oneOf"] = []
    with pytest.raises(fx17.Fx17Error, match="strict root object"):
        fx17.validate_response_schema(schema)


def test_response_parser_accepts_exact_candidate_or_refusal_and_rejects_invalid() -> None:
    candidate = b"#!/bin/sh\nprintf 'ok\\n'\n"
    parsed = fx17.parse_model_response(
        json.dumps({"app_sh": candidate.decode(), "refusal": None}).encode())
    assert parsed == {
        "status": "response_received",
        "candidate": candidate,
        "candidate_sha256": fx17.digest(candidate),
        "candidate_utf8_bytes": len(candidate),
    }
    assert fx17.parse_model_response(
        b'{"app_sh":null,"refusal":"cannot comply"}') == {
            "status": "refusal", "refusal": "cannot comply"}

    for raw in (
        b"not json",
        b'{"app_sh":null,"refusal":null}',
        b'{"app_sh":"script","refusal":"reason"}',
        b'{"app_sh":"script","refusal":null,"extra":0}',
        b'{"app_sh":"bad\\u0000script","refusal":null}',
        b'{"app_sh":"\\ud800","refusal":null}',
    ):
        assert fx17.parse_model_response(raw)["status"] == "invalid_response"


def test_response_parser_rejects_candidate_when_a_tool_call_was_observed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(fx17, "MAX_CANDIDATE_BYTES", 3)
    candidate = b'{"app_sh":"long","refusal":null}'
    assert fx17.parse_model_response(candidate)["status"] == "invalid_response"
    assert fx17.parse_model_response(
        b'{"app_sh":"ok","refusal":null}', {("command_execution", "call-1")}
    )["status"] == "tool_access_violation"


def test_event_classifier_ignores_error_and_counts_unique_tool_items() -> None:
    events = [
        {"type": "item.completed", "item": {"id": "err", "type": "error"}},
        {"type": "item.completed", "item": {"id": "msg", "type": "agent_message"}},
        {"type": "item.started", "item": {"id": "cmd-1", "type": "command_execution"}},
        {"type": "item.completed", "item": {"id": "cmd-1", "type": "command_execution"}},
        {"type": "item.completed", "item": {"id": "mcp-1", "type": "mcp_tool_call"}},
    ]
    calls = fx17.observed_tool_call_ids(events)
    assert calls == {("command_execution", "cmd-1"), ("mcp_tool_call", "mcp-1")}
    assert fx17.observed_tool_call_ids(events[:2]) == set()
