"""Contract checks for `aethyme explore-summary --from <answer-json>`.

Implementation-blind: every assertion drives the built router binary as a
subprocess (tests/support/cli_invoke), so it verifies whichever
implementation answers. The command replaces the `.venv/bin/python`
projection heredoc deployed skill templates used to carry
(python-retirement Phase 5.5) — the assertions below encode the byte
contract that heredoc had, so a future reimplementation cannot silently
drift.
"""

from __future__ import annotations

import json
from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme

# The exact projection the retired heredoc emitted, kept here as the
# oracle: build it in Python, compare bytes with the native command.
_SUBSYSTEM_KEYS = (
    "rank",
    "id",
    "label",
    "role",
    "confidence",
    "token_subsystems",
    "missing_coverage_warnings",
)


def _reference_projection(document: dict) -> str:
    targets: list[dict] = []
    lanes = document.get("subsystems", [])[:3]
    subsystems = [{k: lane.get(k) for k in _SUBSYSTEM_KEYS} for lane in lanes]
    for lane in lanes:
        for target in lane.get("top_verification_targets", [])[:2]:
            if isinstance(target, dict):
                row = dict(target)
                row.setdefault("subsystem", lane.get("role") or lane.get("id"))
                targets.append(row)
    return (
        json.dumps(
            {
                "safe_to_use_as_answer": document.get("safe_to_use_as_answer"),
                "trust_policy": document.get("trust_policy"),
                "subsystems": subsystems,
                "top_verification_targets": targets[:6],
                "verification_steps": document.get("verification_steps", [])[:3],
                "observability": {
                    "readiness": document.get("observability", {}).get("readiness")
                },
            },
            indent=2,
        )
        + "\n"
    )


def _write(tmp_path: Path, document: dict, name: str = "explore.json") -> Path:
    path = tmp_path / name
    path.write_text(json.dumps(document, ensure_ascii=False), encoding="utf-8")
    return path


def _run(path: Path) -> str:
    result = invoke_aethyme(["explore-summary", "--from", str(path)])
    assert result.exit_code == 0, result.output
    return result.output


_FULL_DOCUMENT = {
    "safe_to_use_as_answer": False,
    "trust_policy": "verify-first",
    "ignored_top_level_field": {"dropped": True},
    "subsystems": [
        {
            "rank": 1,
            "id": "ingress",
            "label": "Ingress proxy",
            "role": "edge credential transport",
            "confidence": 0.6666666666666666,
            "token_subsystems": ["proxy"],
            "missing_coverage_warnings": [],
            "top_verification_targets": [
                {"kind": "file", "path": "gcp-run-proxy/src/worker.mjs"},
                {"kind": "symbol", "path": "gcp-run-proxy/src/auth.mjs"},
                {"kind": "symbol", "path": "dropped-third-target"},
            ],
        },
        {
            "rank": 2,
            "id": "backend",
            "role": "",
            "top_verification_targets": [
                {"subsystem": "pinned", "path": "backend/api_keys/middleware.py"},
                "a bare string entry",
            ],
        },
        {
            "rank": 3,
            "id": "storage",
            "top_verification_targets": [{"path": "store/redb.rs"}],
        },
        {"rank": 4, "id": "dropped-fourth-lane"},
    ],
    "verification_steps": ["step one", "step two", "step three", "dropped step"],
    "observability": {"readiness": "degraded", "other": "dropped"},
}


def test_projection_is_byte_identical_to_the_reference(tmp_path: Path) -> None:
    path = _write(tmp_path, _FULL_DOCUMENT)
    assert _run(path) == _reference_projection(_FULL_DOCUMENT)


def test_missing_fields_render_as_null_not_omitted(tmp_path: Path) -> None:
    output = _run(_write(tmp_path, {}))
    assert output == _reference_projection({})
    payload = json.loads(output)
    assert list(payload) == [
        "safe_to_use_as_answer",
        "trust_policy",
        "subsystems",
        "top_verification_targets",
        "verification_steps",
        "observability",
    ]
    assert payload["safe_to_use_as_answer"] is None
    assert payload["trust_policy"] is None
    assert payload["observability"] == {"readiness": None}
    # Contract decision (Phase 5.5): no schema_version key, unlike
    # verify-targets — byte-parity with the retired heredoc keeps the
    # skill's "inspect only these fields" list exactly true.
    assert "schema_version" not in payload


def test_truncation_and_subsystem_tagging(tmp_path: Path) -> None:
    payload = json.loads(_run(_write(tmp_path, _FULL_DOCUMENT)))

    assert len(payload["subsystems"]) == 3
    for lane in payload["subsystems"]:
        assert list(lane) == list(_SUBSYSTEM_KEYS)
    assert payload["subsystems"][2]["label"] is None

    targets = payload["top_verification_targets"]
    assert [t["path"] for t in targets] == [
        "gcp-run-proxy/src/worker.mjs",
        "gcp-run-proxy/src/auth.mjs",
        "backend/api_keys/middleware.py",
        "store/redb.rs",
    ]
    # role wins when truthy; an existing subsystem key is left alone; an
    # empty-string role falls through to the lane id.
    assert targets[0]["subsystem"] == "edge credential transport"
    assert targets[2]["subsystem"] == "pinned"
    assert targets[3]["subsystem"] == "storage"
    # `subsystem` is appended last when the projection adds it.
    assert list(targets[0])[-1] == "subsystem"

    assert payload["verification_steps"] == ["step one", "step two", "step three"]


def test_overall_target_cap_is_six(tmp_path: Path) -> None:
    document = {
        "subsystems": [
            {
                "id": f"lane{n}",
                "top_verification_targets": [
                    {"path": f"l{n}-t{k}"} for k in range(1, 4)
                ],
            }
            for n in range(1, 5)
        ]
    }
    payload = json.loads(_run(_write(tmp_path, document)))
    assert len(payload["top_verification_targets"]) == 6


def test_non_ascii_is_escaped_like_cpython_ensure_ascii(tmp_path: Path) -> None:
    document = {
        "trust_policy": "vérifier",
        "subsystems": [
            {
                "id": "café",
                "label": "日本語",
                "role": "",
                "top_verification_targets": [{"path": "src/naïve/🚀.py"}],
            }
        ],
        "observability": {"readiness": "dégradé"},
    }
    path = _write(tmp_path, document)
    output = _run(path)
    assert output == _reference_projection(document)
    assert output.isascii(), output
    assert "\\u00e9rifier" in output
    assert "\\ud83d\\ude80" in output  # astral char as a surrogate pair
    assert json.loads(output)["subsystems"][0]["label"] == "日本語"


def test_reads_from_stdin_with_dash(tmp_path: Path) -> None:
    path = _write(tmp_path, _FULL_DOCUMENT)
    # `--from -` is the same reader idiom verify-targets exposes.
    result = invoke_aethyme(["explore-summary", "--from", "-"], stdin=path.read_text())
    assert result.exit_code == 0, result.output
    assert result.output == _reference_projection(_FULL_DOCUMENT)


def test_missing_from_option_is_a_usage_error() -> None:
    result = invoke_aethyme(["explore-summary"])
    assert result.exit_code == 2, result.output
    assert "--from" in result.output


def test_unparseable_json_is_a_usage_error(tmp_path: Path) -> None:
    path = tmp_path / "broken.json"
    path.write_text("not json at all", encoding="utf-8")
    result = invoke_aethyme(["explore-summary", "--from", str(path)])
    assert result.exit_code == 2, result.output
    assert "parse --from JSON" in result.output


def test_non_object_document_is_a_usage_error(tmp_path: Path) -> None:
    path = tmp_path / "array.json"
    path.write_text("[]", encoding="utf-8")
    result = invoke_aethyme(["explore-summary", "--from", str(path)])
    assert result.exit_code == 2, result.output
    assert "must be an object" in result.output


def test_missing_file_fails(tmp_path: Path) -> None:
    result = invoke_aethyme(["explore-summary", "--from", str(tmp_path / "nope.json")])
    assert result.exit_code == 1, result.output
    assert "read --from" in result.output


def test_listed_in_router_help() -> None:
    result = invoke_aethyme(["--help"])
    assert result.exit_code == 0, result.output
    assert "explore-summary --from" in result.output
