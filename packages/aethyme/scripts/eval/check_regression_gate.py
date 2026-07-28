#!/usr/bin/env python3
"""Compare Control vs Aethyme playground runs using stable regression metrics."""

from __future__ import annotations

import argparse
import json
import shlex
import sys
from pathlib import Path
from typing import Any

DEFAULT_TOKEN_DELTA_RATIO = 0.20
DEFAULT_SELECTED_FILE_DELTA_RATIO = 0.50
DEFAULT_SNIPPET_DELTA_RATIO = 0.50
DEFAULT_COMMAND_OUTPUT_DELTA_RATIO = 0.25
COMMAND_FIELD_KEYS = {"cmd", "command"}
REQUIRED_INT_METRICS = (
    "token_estimate",
    "selected_file_count",
    "snippet_count",
    "command_output_chars",
)
REQUIRED_BOOL_METRICS = ("aethyme_path_leaked", "aethyme_invoked")


def main() -> int:
    args = _parse_args()
    control = _read_json(args.control)
    aethyme = _read_json(args.aethyme)

    report = compare_runs(
        control,
        aethyme,
        control_quality=args.control_quality,
        aethyme_quality=args.aethyme_quality,
        token_delta_ratio=args.max_token_delta_ratio,
        token_slack=args.token_slack,
        selected_file_delta_ratio=args.max_selected_file_delta_ratio,
        selected_file_slack=args.selected_file_slack,
        snippet_delta_ratio=args.max_snippet_delta_ratio,
        snippet_slack=args.snippet_slack,
        command_output_delta_ratio=args.max_command_output_delta_ratio,
        command_output_slack=args.command_output_slack,
        allow_missing_quality=args.allow_missing_quality,
    )
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


def compare_runs(
    control: dict[str, Any],
    aethyme: dict[str, Any],
    *,
    control_quality: float | None = None,
    aethyme_quality: float | None = None,
    token_delta_ratio: float = DEFAULT_TOKEN_DELTA_RATIO,
    token_slack: int = 512,
    selected_file_delta_ratio: float = DEFAULT_SELECTED_FILE_DELTA_RATIO,
    selected_file_slack: int = 2,
    snippet_delta_ratio: float = DEFAULT_SNIPPET_DELTA_RATIO,
    snippet_slack: int = 2,
    command_output_delta_ratio: float = DEFAULT_COMMAND_OUTPUT_DELTA_RATIO,
    command_output_slack: int = 1024,
    allow_missing_quality: bool = False,
) -> dict[str, Any]:
    control_metrics = _metrics(control)
    aethyme_metrics = _metrics(aethyme)
    checks = [
        *_metric_contract_checks("control", control_metrics),
        *_metric_contract_checks("aethyme", aethyme_metrics),
        _boolean_check(
            "control_did_not_invoke_aethyme",
            not bool(control_metrics.get("aethyme_invoked")),
            "Control arm must not invoke Aethyme.",
        ),
        _boolean_check(
            "aethyme_invoked",
            bool(aethyme_metrics.get("aethyme_invoked")),
            "Aethyme arm must invoke the intended Aethyme surface.",
        ),
        _boolean_check(
            "control_no_aethyme_path_leak",
            not bool(control_metrics.get("aethyme_path_leaked")),
            "Control arm leaked a .aethyme path.",
        ),
        _boolean_check(
            "aethyme_no_aethyme_path_leak",
            not bool(aethyme_metrics.get("aethyme_path_leaked")),
            "Aethyme arm leaked a .aethyme path.",
        ),
        _delta_check(
            "token_estimate_delta",
            _metric_int(aethyme_metrics, "token_estimate"),
            _metric_int(control_metrics, "token_estimate"),
            token_delta_ratio,
            token_slack,
        ),
        _delta_check(
            "selected_file_count_delta",
            _metric_int(aethyme_metrics, "selected_file_count"),
            _metric_int(control_metrics, "selected_file_count"),
            selected_file_delta_ratio,
            selected_file_slack,
        ),
        _delta_check(
            "snippet_count_delta",
            _metric_int(aethyme_metrics, "snippet_count"),
            _metric_int(control_metrics, "snippet_count"),
            snippet_delta_ratio,
            snippet_slack,
        ),
        _delta_check(
            "command_output_char_delta",
            _metric_int(aethyme_metrics, "command_output_chars"),
            _metric_int(control_metrics, "command_output_chars"),
            command_output_delta_ratio,
            command_output_slack,
        ),
        _quality_check(
            _quality_score(control, control_metrics, control_quality),
            _quality_score(aethyme, aethyme_metrics, aethyme_quality),
            allow_missing=allow_missing_quality,
        ),
    ]
    return {
        "passed": all(check["passed"] for check in checks),
        "checks": checks,
        "control_metrics": control_metrics,
        "aethyme_metrics": aethyme_metrics,
        "contract": {
            "selected_file_contents_compared": False,
            "selected_file_count_compared": True,
            "quality_source": "reviewer rubric or supplied quality_score field",
        },
    }


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--control", required=True, type=Path, help="Control runner JSON")
    parser.add_argument("--aethyme", required=True, type=Path, help="Aethyme runner JSON")
    parser.add_argument("--control-quality", type=float, default=None)
    parser.add_argument("--aethyme-quality", type=float, default=None)
    parser.add_argument("--max-token-delta-ratio", type=float, default=DEFAULT_TOKEN_DELTA_RATIO)
    parser.add_argument("--token-slack", type=int, default=512)
    parser.add_argument(
        "--max-selected-file-delta-ratio",
        type=float,
        default=DEFAULT_SELECTED_FILE_DELTA_RATIO,
    )
    parser.add_argument("--selected-file-slack", type=int, default=2)
    parser.add_argument(
        "--max-snippet-delta-ratio",
        type=float,
        default=DEFAULT_SNIPPET_DELTA_RATIO,
    )
    parser.add_argument("--snippet-slack", type=int, default=2)
    parser.add_argument(
        "--max-command-output-delta-ratio",
        type=float,
        default=DEFAULT_COMMAND_OUTPUT_DELTA_RATIO,
    )
    parser.add_argument("--command-output-slack", type=int, default=1024)
    parser.add_argument(
        "--allow-missing-quality",
        action="store_true",
        help="Report missing reviewer quality as skipped instead of failing.",
    )
    return parser.parse_args()


def _read_json(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit(f"{path} did not contain a JSON object")
    return payload


def _metrics(payload: dict[str, Any]) -> dict[str, Any]:
    metrics = payload.get("regression_metrics")
    if isinstance(metrics, dict):
        return dict(metrics)
    structured_output = payload.get("structured_output")
    return {
        "token_estimate": _token_estimate(payload),
        "selected_file_count": _count_list_field(structured_output, "selected_files"),
        "snippet_count": _count_list_field(structured_output, "snippets"),
        "command_output_chars": payload.get("command_output_chars"),
        "aethyme_path_leaked": bool(
            payload.get("artifact_leakage", {}).get("aethyme_path_leaked")
        ),
        "aethyme_invoked": _aethyme_invoked(payload),
    }


def _token_estimate(payload: dict[str, Any]) -> int | None:
    input_tokens = payload.get("input_tokens")
    output_tokens = payload.get("output_tokens")
    if type(input_tokens) is int and type(output_tokens) is int:
        return input_tokens + output_tokens
    event_log_chars = payload.get("event_log_chars")
    if type(event_log_chars) is int:
        return (max(event_log_chars, 0) + 3) // 4
    return None


def _count_list_field(value: Any, field_name: str) -> int:
    if isinstance(value, dict):
        total = 0
        for key, item in value.items():
            if key == field_name and isinstance(item, list):
                total += len(item)
            else:
                total += _count_list_field(item, field_name)
        return total
    if isinstance(value, list):
        return sum(_count_list_field(item, field_name) for item in value)
    return 0


def _metric_int(metrics: dict[str, Any], key: str) -> int:
    return _int_or_zero(metrics.get(key))


def _int_or_zero(value: Any) -> int:
    return value if type(value) is int else 0


def _metric_contract_checks(label: str, metrics: dict[str, Any]) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    for key in REQUIRED_INT_METRICS:
        value = metrics.get(key)
        passed = type(value) is int and value >= 0
        checks.append(
            {
                "name": f"{label}_metric_{key}_valid",
                "passed": passed,
                "value": value,
                "failure": None if passed else f"{label} metric {key} must be a non-negative int",
            }
        )
    for key in REQUIRED_BOOL_METRICS:
        value = metrics.get(key)
        passed = type(value) is bool
        checks.append(
            {
                "name": f"{label}_metric_{key}_valid",
                "passed": passed,
                "value": value,
                "failure": None if passed else f"{label} metric {key} must be a bool",
            }
        )
    return checks


def _aethyme_invoked(payload: dict[str, Any]) -> bool:
    event_log_file = payload.get("event_log_file")
    if not isinstance(event_log_file, str):
        return False
    path = Path(event_log_file)
    if not path.is_file():
        return False
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if _event_contains_aethyme_invocation(event):
            return True
    return False


def _event_contains_aethyme_invocation(value: Any, *, key: str | None = None) -> bool:
    if isinstance(value, str):
        if key in COMMAND_FIELD_KEYS:
            return _command_tokens_invoke_aethyme_explore(_split_command_text(value))
        return False
    if isinstance(value, list):
        if key in COMMAND_FIELD_KEYS:
            return _command_tokens_invoke_aethyme_explore(value)
        return any(_event_contains_aethyme_invocation(item) for item in value)
    if isinstance(value, dict):
        return any(
            _event_contains_aethyme_invocation(item, key=str(item_key))
            for item_key, item in value.items()
        )
    return False


def _split_command_text(value: str) -> list[str]:
    try:
        return shlex.split(value)
    except ValueError:
        return value.split()


def _command_tokens_invoke_aethyme_explore(value: list[Any]) -> bool:
    tokens = [item for item in value if isinstance(item, str)]
    if not tokens:
        return False
    has_aethyme_binary = any(_is_aethyme_binary(token) for token in tokens)
    has_explore_subcommand = any(token.lower() == "explore" for token in tokens)
    return has_aethyme_binary and has_explore_subcommand


def _is_aethyme_binary(token: str) -> bool:
    return Path(token).name.lower() in {"aethyme", "aethyme-engine-cli"}


def _boolean_check(name: str, passed: bool, failure: str) -> dict[str, Any]:
    return {
        "name": name,
        "passed": passed,
        "failure": None if passed else failure,
    }


def _delta_check(
    name: str,
    actual: int,
    baseline: int,
    max_delta_ratio: float,
    slack: int,
) -> dict[str, Any]:
    limit = int(baseline * (1.0 + max_delta_ratio)) + slack
    passed = actual <= limit
    return {
        "name": name,
        "passed": passed,
        "baseline": baseline,
        "actual": actual,
        "delta": actual - baseline,
        "limit": limit,
        "max_delta_ratio": max_delta_ratio,
        "slack": slack,
        "failure": None if passed else f"{actual} exceeded limit {limit}",
    }


def _quality_score(
    payload: dict[str, Any],
    metrics: dict[str, Any],
    override: float | None,
) -> float | None:
    if override is not None:
        return override
    for key in ("reviewer_quality_score", "quality_score", "final_answer_quality"):
        value = payload.get(key)
        if isinstance(value, (int, float)):
            return float(value)
        metric_value = metrics.get(key)
        if isinstance(metric_value, (int, float)):
            return float(metric_value)
    return None


def _quality_check(
    control_quality: float | None,
    aethyme_quality: float | None,
    *,
    allow_missing: bool,
) -> dict[str, Any]:
    if control_quality is None or aethyme_quality is None:
        return {
            "name": "final_answer_quality_not_worse",
            "passed": allow_missing,
            "skipped": allow_missing,
            "control_quality": control_quality,
            "aethyme_quality": aethyme_quality,
            "failure": None
            if allow_missing
            else "missing reviewer quality score for one or both arms",
        }
    passed = aethyme_quality >= control_quality
    return {
        "name": "final_answer_quality_not_worse",
        "passed": passed,
        "skipped": False,
        "control_quality": control_quality,
        "aethyme_quality": aethyme_quality,
        "delta": aethyme_quality - control_quality,
        "failure": None if passed else "Aethyme reviewer quality was worse than Control",
    }


if __name__ == "__main__":
    sys.exit(main())
