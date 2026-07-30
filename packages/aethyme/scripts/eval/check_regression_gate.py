#!/usr/bin/env python3
"""Compare Control vs Aethyme playground runs using stable regression metrics."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shlex
import sys
from pathlib import Path
from typing import Any

DEFAULT_TOKEN_DELTA_RATIO = 0.0
DEFAULT_SELECTED_FILE_DELTA_RATIO = 0.50
DEFAULT_SNIPPET_DELTA_RATIO = 0.50
DEFAULT_COMMAND_OUTPUT_DELTA_RATIO = 0.25
DEFAULT_MAX_COMMAND_OUTPUT_CHARS = 64_000
DEFAULT_MAX_SINGLE_COMMAND_OUTPUT_CHARS = 20_000
DEFAULT_MAX_EXPLORE_OUTPUT_CHARS = 20_000
DEFAULT_MAX_CUMULATIVE_REPLAY_ESTIMATE = 50_000
COMMAND_FIELD_KEYS = {"cmd", "command"}
COMMAND_OUTPUT_KEYS = {"aggregated_output", "output", "stdout", "stderr"}
AUTH_TOKEN_FIXTURE_IDS = {
    "django_backend_auth",
    "edge_proxy_backend_auth",
    "oidc_session_auth",
    "webhook_secret_auth",
    "config_owned_middleware_behavior",
    "frontend_backend_route_behavior",
}
AUTH_TOKEN_TASK_KEYWORDS = (
    "api key",
    "api_key",
    "apikey",
    "auth",
    "authorization",
    "bearer",
    "credential",
    "jwt",
    "jws",
    "oauth",
    "oidc",
    "session",
    "token",
    "webhook",
)
AUTH_SURFACE_LANE_ROLES = {
    "backend_validator",
    "ingress_proxy",
    "provider_or_secondary_token",
}
AUTH_SURFACE_LANE_ROLE_ALIASES = {
    "authoritative_pk_validator": "backend_validator",
    "claims_pk_bearer_tokens_and_establishes_request_identity_and_tenant_context": "backend_validator",
    "django_api_key_authentication": "backend_validator",
    "validates_key_material_active_state_and_expiry": "backend_validator",
    "classifies_and_forwards_strips_or_rewrites_inbound_authorization": "ingress_proxy",
    "cloudflare_cloud_run_ingress_proxy": "ingress_proxy",
    "cloudflare_worker": "ingress_proxy",
    "edge_credential_transport": "ingress_proxy",
    "decoy_or_secondary_token_subsystems": "provider_or_secondary_token",
    "parallel_oauth_path_not_a_second_pk_validator": "provider_or_secondary_token",
}
AUTH_FIXTURE_REQUIRED_LANES = {
    "django_backend_auth": {"backend_validator"},
    "edge_proxy_backend_auth": {"backend_validator", "ingress_proxy"},
}
PLAYGROUND_FIXTURE_CADENCE = (
    ("edge_proxy_backend_auth", "edge proxy + backend auth"),
    ("django_backend_auth", "Django backend-only auth"),
    ("oidc_session_auth", "OIDC + session auth"),
    ("webhook_secret_auth", "webhook secret auth"),
    ("config_owned_middleware_behavior", "config-owned middleware behavior"),
    ("frontend_backend_route_behavior", "frontend-to-backend route behavior"),
    ("queue_job_behavior", "queue/job behavior"),
)
REQUIRED_PLAYGROUND_FIXTURES = dict(PLAYGROUND_FIXTURE_CADENCE)
REQUIRED_PLAYGROUND_FIXTURE_ORDER = tuple(
    fixture_id for fixture_id, _label in PLAYGROUND_FIXTURE_CADENCE
)
REQUIRED_INT_METRICS = (
    "token_estimate",
    "total_input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "uncached_input_tokens",
    "uncached_plus_output_tokens",
    "selected_file_count",
    "snippet_count",
    "command_output_chars",
    "max_single_command_output_chars",
    "explore_output_chars",
    "cumulative_replay_token_estimate",
)
REQUIRED_BOOL_METRICS = (
    "generated_artifact_leaked",
    "aethyme_path_leaked",
    "aethyme_invoked",
    "broad_rg_after_successful_explore",
    "successful_explore_detected",
)
NONDETERMINISTIC_OUTPUT_FIELDS = {
    "artifact_dir",
    "artifact_leakage",
    "arm",
    "command_output_chars",
    "event_log_file",
    "fixture_id",
    "stderr_file",
    "last_message_file",
    "leakage_file",
    "output_fingerprint",
    "regression_metrics",
    "wall_time_seconds",
    "event_log_chars",
    "stderr_chars",
    "input_tokens",
    "total_input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "uncached_input_tokens",
    "uncached_plus_output_tokens",
    "cache_write_input_tokens",
    "retries",
    "review_burden",
    "runner_settings",
    "contract",
}


def main() -> int:
    args = _parse_args()
    if args.suite:
        suite = _read_json(args.suite)
        report = compare_suite(
            suite,
            suite_root=args.suite.parent,
            token_delta_ratio=args.max_token_delta_ratio,
            token_slack=args.token_slack,
            selected_file_delta_ratio=args.max_selected_file_delta_ratio,
            selected_file_slack=args.selected_file_slack,
            snippet_delta_ratio=args.max_snippet_delta_ratio,
            snippet_slack=args.snippet_slack,
            command_output_delta_ratio=args.max_command_output_delta_ratio,
            command_output_slack=args.command_output_slack,
            max_command_output_chars=args.max_command_output_chars,
            max_single_command_output_chars=args.max_single_command_output_chars,
            max_explore_output_chars=args.max_explore_output_chars,
            max_cumulative_replay_estimate=args.max_cumulative_replay_estimate,
            control_quality=args.control_quality,
            aethyme_quality=args.aethyme_quality,
            allow_missing_quality=args.allow_missing_quality,
            require_playground_contract=not args.allow_missing_playground_contract,
            require_determinism=not args.allow_missing_determinism,
            require_coverage_report=not args.allow_missing_coverage_report,
            require_event_sequence=not args.allow_missing_event_sequence,
            require_auth_surface_lanes=not args.allow_missing_auth_surface_lanes,
        )
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0 if report["passed"] else 1

    if args.control is None or args.aethyme is None:
        raise SystemExit("Either --suite or both --control and --aethyme are required")

    control = _read_json(args.control)
    aethyme = _read_json(args.aethyme)
    control_repeat = _read_json(args.control_repeat) if args.control_repeat else None
    aethyme_repeat = _read_json(args.aethyme_repeat) if args.aethyme_repeat else None

    report = compare_runs(
        control,
        aethyme,
        control_repeat=control_repeat,
        aethyme_repeat=aethyme_repeat,
        fixture_id=args.fixture,
        expected_missing_coverage=_split_values(args.expected_missing_coverage),
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
        max_command_output_chars=args.max_command_output_chars,
        max_single_command_output_chars=args.max_single_command_output_chars,
        max_explore_output_chars=args.max_explore_output_chars,
        max_cumulative_replay_estimate=args.max_cumulative_replay_estimate,
        allow_missing_quality=args.allow_missing_quality,
        require_playground_contract=not args.allow_missing_playground_contract,
        require_determinism=not args.allow_missing_determinism,
        require_coverage_report=not args.allow_missing_coverage_report,
        require_event_sequence=not args.allow_missing_event_sequence,
        require_auth_surface_lanes=not args.allow_missing_auth_surface_lanes,
    )
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


def compare_runs(
    control: dict[str, Any],
    aethyme: dict[str, Any],
    *,
    control_repeat: dict[str, Any] | None = None,
    aethyme_repeat: dict[str, Any] | None = None,
    fixture_id: str | None = None,
    expected_missing_coverage: list[str] | None = None,
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
    max_command_output_chars: int | None = None,
    max_single_command_output_chars: int | None = DEFAULT_MAX_SINGLE_COMMAND_OUTPUT_CHARS,
    max_explore_output_chars: int | None = DEFAULT_MAX_EXPLORE_OUTPUT_CHARS,
    max_cumulative_replay_estimate: int | None = DEFAULT_MAX_CUMULATIVE_REPLAY_ESTIMATE,
    allow_missing_quality: bool = False,
    require_playground_contract: bool = False,
    require_determinism: bool = False,
    require_coverage_report: bool = False,
    require_event_sequence: bool = False,
    require_auth_surface_lanes: bool = True,
) -> dict[str, Any]:
    control_metrics = _metrics(control)
    aethyme_metrics = _metrics(aethyme)
    resolved_fixture_id = _resolve_pair_fixture_id(control, aethyme, fixture_id)
    resolved_expected_missing = expected_missing_coverage or _expected_missing_coverage(aethyme)
    checks = [
        *_metric_contract_checks("control", control_metrics),
        *_metric_contract_checks("aethyme", aethyme_metrics),
        *_playground_contract_checks("control", control, require=require_playground_contract),
        *_playground_contract_checks("aethyme", aethyme, require=require_playground_contract),
        _fixture_check(resolved_fixture_id),
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
            "control_no_generated_artifact_leak",
            not bool(control_metrics.get("generated_artifact_leaked")),
            "Control arm leaked generated Aethyme scaffolding.",
        ),
        _boolean_check(
            "aethyme_no_generated_artifact_leak",
            not bool(aethyme_metrics.get("generated_artifact_leaked")),
            "Aethyme arm leaked generated Aethyme scaffolding.",
        ),
        *_command_output_bound_checks(control_metrics, aethyme_metrics, max_command_output_chars),
        *_single_command_output_bound_checks(
            control_metrics,
            aethyme_metrics,
            max_single_command_output_chars,
        ),
        _explore_output_bound_check(aethyme_metrics, max_explore_output_chars),
        *_cumulative_replay_bound_checks(
            control_metrics,
            aethyme_metrics,
            max_cumulative_replay_estimate,
        ),
        _first_aethyme_before_broad_search_check(
            aethyme_metrics,
            require=require_event_sequence,
        ),
        _broad_rg_after_successful_explore_warning(aethyme_metrics),
        _surface_flow_lanes_check(
            aethyme,
            aethyme_metrics,
            fixture_id=resolved_fixture_id,
            require=require_auth_surface_lanes,
        ),
        _token_estimate_delta_observation(
            control_metrics,
            aethyme_metrics,
            token_delta_ratio,
            token_slack,
        ),
        _delta_check(
            "uncached_plus_output_budget_delta",
            _metric_int(aethyme_metrics, "uncached_plus_output_tokens"),
            _metric_int(control_metrics, "uncached_plus_output_tokens"),
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
        *_determinism_checks(
            control,
            aethyme,
            control_repeat,
            aethyme_repeat,
            require=require_determinism,
        ),
        _coverage_report_check(
            aethyme,
            expected_missing_coverage=resolved_expected_missing,
            require=require_coverage_report,
        ),
    ]
    return {
        "passed": all(check["passed"] for check in checks),
        "checks": checks,
        "control_metrics": control_metrics,
        "aethyme_metrics": aethyme_metrics,
        "fixture_id": resolved_fixture_id,
        "expected_missing_coverage": resolved_expected_missing,
        "contract": {
            "selected_file_contents_compared": False,
            "selected_file_count_compared": True,
            "token_estimate_compared": False,
            "budget_token_metric": "uncached_plus_output_tokens",
            "token_estimate_role": "context-pressure telemetry only",
            "required_playground_fixtures": REQUIRED_PLAYGROUND_FIXTURES,
            "playground_fixture_cadence": list(REQUIRED_PLAYGROUND_FIXTURE_ORDER),
            "strict_playground_contract": require_playground_contract,
            "determinism_requires_repeat_result": require_determinism,
            "coverage_report_required": require_coverage_report,
            "max_command_output_chars": max_command_output_chars,
            "max_single_command_output_chars": max_single_command_output_chars,
            "max_explore_output_chars": max_explore_output_chars,
            "max_cumulative_replay_estimate": max_cumulative_replay_estimate,
            "event_sequence_required": require_event_sequence,
            "auth_surface_lanes_required": require_auth_surface_lanes,
            "quality_source": "reviewer rubric or supplied quality_score field",
        },
    }


def compare_suite(
    suite: dict[str, Any],
    *,
    suite_root: Path | None = None,
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
    max_command_output_chars: int | None = DEFAULT_MAX_COMMAND_OUTPUT_CHARS,
    max_single_command_output_chars: int | None = DEFAULT_MAX_SINGLE_COMMAND_OUTPUT_CHARS,
    max_explore_output_chars: int | None = DEFAULT_MAX_EXPLORE_OUTPUT_CHARS,
    max_cumulative_replay_estimate: int | None = DEFAULT_MAX_CUMULATIVE_REPLAY_ESTIMATE,
    allow_missing_quality: bool = False,
    require_playground_contract: bool = True,
    require_determinism: bool = True,
    require_coverage_report: bool = True,
    require_event_sequence: bool = True,
    require_auth_surface_lanes: bool = True,
) -> dict[str, Any]:
    entries = _suite_entries(suite)
    fixture_ids = [_normalize_fixture_id(str(entry.get("fixture_id", ""))) for entry in entries]
    fixture_checks = [
        *_required_fixture_checks(fixture_ids),
        _fixture_cadence_order_check(fixture_ids),
    ]
    run_reports: list[dict[str, Any]] = []

    for index, entry in enumerate(entries):
        fixture_id = _normalize_fixture_id(str(entry.get("fixture_id", ""))) or None
        control = _read_result_spec(entry.get("control"), suite_root)
        aethyme = _read_result_spec(entry.get("aethyme"), suite_root)
        control_repeat = _read_result_spec(entry.get("control_repeat"), suite_root, optional=True)
        aethyme_repeat = _read_result_spec(entry.get("aethyme_repeat"), suite_root, optional=True)
        expected_missing = _split_values(entry.get("expected_missing_coverage"))
        report = compare_runs(
            control,
            aethyme,
            control_repeat=control_repeat,
            aethyme_repeat=aethyme_repeat,
            fixture_id=fixture_id,
            expected_missing_coverage=expected_missing,
            control_quality=_float_or_none(entry.get("control_quality"), control_quality),
            aethyme_quality=_float_or_none(entry.get("aethyme_quality"), aethyme_quality),
            token_delta_ratio=token_delta_ratio,
            token_slack=token_slack,
            selected_file_delta_ratio=selected_file_delta_ratio,
            selected_file_slack=selected_file_slack,
            snippet_delta_ratio=snippet_delta_ratio,
            snippet_slack=snippet_slack,
            command_output_delta_ratio=command_output_delta_ratio,
            command_output_slack=command_output_slack,
            max_command_output_chars=max_command_output_chars,
            max_single_command_output_chars=max_single_command_output_chars,
            max_explore_output_chars=max_explore_output_chars,
            max_cumulative_replay_estimate=max_cumulative_replay_estimate,
            allow_missing_quality=allow_missing_quality,
            require_playground_contract=require_playground_contract,
            require_determinism=require_determinism,
            require_coverage_report=require_coverage_report,
            require_event_sequence=require_event_sequence,
            require_auth_surface_lanes=require_auth_surface_lanes,
        )
        report["suite_index"] = index
        run_reports.append(report)

    checks = [*fixture_checks]
    for report in run_reports:
        checks.append(
            {
                "name": f"suite_run_{report['suite_index']}_passed",
                "fixture_id": report.get("fixture_id"),
                "passed": report["passed"],
                "failure": None if report["passed"] else "fixture run failed pair checks",
            }
        )

    return {
        "passed": all(check["passed"] for check in checks),
        "checks": checks,
        "runs": run_reports,
        "required_fixtures": REQUIRED_PLAYGROUND_FIXTURES,
        "fixture_cadence": list(REQUIRED_PLAYGROUND_FIXTURE_ORDER),
        "present_fixtures": sorted({fixture_id for fixture_id in fixture_ids if fixture_id}),
        "present_fixtures_in_suite_order": [fixture_id for fixture_id in fixture_ids if fixture_id],
    }


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", type=Path, default=None, help="Suite manifest JSON")
    parser.add_argument("--control", type=Path, default=None, help="Control runner JSON")
    parser.add_argument("--aethyme", type=Path, default=None, help="Aethyme runner JSON")
    parser.add_argument("--control-repeat", type=Path, default=None, help="Repeat Control JSON")
    parser.add_argument("--aethyme-repeat", type=Path, default=None, help="Repeat Aethyme JSON")
    parser.add_argument("--fixture", default=None, help="Required playground fixture id")
    parser.add_argument(
        "--expected-missing-coverage",
        action="append",
        default=[],
        help="Surface type that should be reported as missing; may be repeated or comma-separated.",
    )
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
        "--max-command-output-chars", type=int, default=DEFAULT_MAX_COMMAND_OUTPUT_CHARS
    )
    parser.add_argument(
        "--max-single-command-output-chars",
        type=int,
        default=DEFAULT_MAX_SINGLE_COMMAND_OUTPUT_CHARS,
        help="Absolute cap for any one command's emitted stdout/stderr/aggregated output.",
    )
    parser.add_argument(
        "--max-explore-output-chars",
        type=int,
        default=DEFAULT_MAX_EXPLORE_OUTPUT_CHARS,
        help="Absolute cap for captured output from Aethyme Explore commands.",
    )
    parser.add_argument(
        "--max-cumulative-replay-estimate",
        type=int,
        default=DEFAULT_MAX_CUMULATIVE_REPLAY_ESTIMATE,
        help="Estimated token cap for replaying captured eval artifacts.",
    )
    parser.add_argument(
        "--allow-missing-quality",
        action="store_true",
        help="Report missing reviewer quality as skipped instead of failing.",
    )
    parser.add_argument(
        "--allow-missing-playground-contract",
        action="store_true",
        help="Allow legacy result JSON that lacks the playground/self-eval contract.",
    )
    parser.add_argument(
        "--allow-missing-determinism",
        action="store_true",
        help="Allow runs without repeat result JSON for deterministic-output comparison.",
    )
    parser.add_argument(
        "--allow-missing-coverage-report",
        action="store_true",
        help="Allow Aethyme results that omit Surface/Flow coverage observability.",
    )
    parser.add_argument(
        "--allow-missing-event-sequence",
        action="store_true",
        help="Allow results without event-log evidence for Aethyme-before-broad-search checks.",
    )
    parser.add_argument(
        "--allow-missing-auth-surface-lanes",
        action="store_true",
        help="Allow auth/token tasks that omit Surface/Flow subsystem lanes.",
    )
    return parser.parse_args()


def _read_json(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit(f"{path} did not contain a JSON object")
    return payload


def _suite_entries(suite: dict[str, Any]) -> list[dict[str, Any]]:
    raw_entries = suite.get("runs", suite.get("results", suite.get("pairs")))
    if not isinstance(raw_entries, list):
        raise SystemExit("Suite manifest must contain a runs/results/pairs array")
    entries: list[dict[str, Any]] = []
    for index, raw_entry in enumerate(raw_entries):
        if not isinstance(raw_entry, dict):
            raise SystemExit(f"Suite entry {index} must be a JSON object")
        entries.append(raw_entry)
    return entries


def _read_result_spec(
    value: Any,
    suite_root: Path | None,
    *,
    optional: bool = False,
) -> dict[str, Any] | None:
    if value is None and optional:
        return None
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        path = Path(value).expanduser()
        if not path.is_absolute() and suite_root is not None:
            path = suite_root / path
        return _read_json(path)
    raise SystemExit("Suite result entries must be JSON objects or file paths")


def _required_fixture_checks(fixture_ids: list[str]) -> list[dict[str, Any]]:
    present = {fixture_id for fixture_id in fixture_ids if fixture_id}
    checks: list[dict[str, Any]] = []
    for fixture_id, label in REQUIRED_PLAYGROUND_FIXTURES.items():
        passed = fixture_id in present
        checks.append(
            {
                "name": f"required_fixture_{fixture_id}_present",
                "fixture_id": fixture_id,
                "label": label,
                "passed": passed,
                "failure": None if passed else f"missing required fixture: {label}",
            }
        )
    unknown = sorted(present - set(REQUIRED_PLAYGROUND_FIXTURES))
    checks.append(
        {
            "name": "no_unknown_fixture_ids",
            "passed": not unknown,
            "unknown_fixture_ids": unknown,
            "failure": None if not unknown else f"unknown fixture ids: {', '.join(unknown)}",
        }
    )
    return checks


def _fixture_cadence_order_check(fixture_ids: list[str]) -> dict[str, Any]:
    known_order = [
        fixture_id for fixture_id in fixture_ids if fixture_id in REQUIRED_PLAYGROUND_FIXTURES
    ]
    positions = {
        fixture_id: index for index, fixture_id in enumerate(REQUIRED_PLAYGROUND_FIXTURE_ORDER)
    }
    expected_for_present = sorted(known_order, key=lambda fixture_id: positions[fixture_id])
    passed = known_order == expected_for_present
    return {
        "name": "fixture_cadence_order",
        "passed": passed,
        "expected_order": list(REQUIRED_PLAYGROUND_FIXTURE_ORDER),
        "expected_order_for_present_fixtures": expected_for_present,
        "actual_order": known_order,
        "failure": None
        if passed
        else "fixture suite order does not match canonical playground evaluation cadence",
    }


def _float_or_none(value: Any, fallback: float | None) -> float | None:
    if isinstance(value, (int, float)):
        return float(value)
    return fallback


def _metrics(payload: dict[str, Any]) -> dict[str, Any]:
    metrics = payload.get("regression_metrics")
    if isinstance(metrics, dict):
        return _normalized_metrics(dict(metrics), payload)
    structured_output = payload.get("structured_output")
    return _normalized_metrics(
        {
            "token_estimate": _token_estimate(payload),
            "selected_file_count": _count_list_field(structured_output, "selected_files"),
            "snippet_count": _count_list_field(structured_output, "snippets"),
            "command_output_chars": payload.get("command_output_chars"),
            "aethyme_path_leaked": _artifact_leak_bool(payload, "aethyme_path_leaked"),
            "aethyme_invoked": _aethyme_invoked(payload),
        },
        payload,
    )


def _normalized_metrics(metrics: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
    _normalize_token_metrics(metrics, payload)
    event_analysis = _event_log_analysis(payload)
    artifact_leakage = payload.get("artifact_leakage")
    generated_leak = metrics.get("generated_artifact_leaked")
    if type(generated_leak) is not bool and isinstance(artifact_leakage, dict):
        generated_leak = artifact_leakage.get("generated_artifact_leaked")
    if type(generated_leak) is not bool:
        generated_leak = metrics.get("aethyme_path_leaked")
    if type(generated_leak) is not bool and isinstance(artifact_leakage, dict):
        generated_leak = artifact_leakage.get("aethyme_path_leaked")
    if type(generated_leak) is bool:
        metrics["generated_artifact_leaked"] = generated_leak
        metrics.setdefault("aethyme_path_leaked", generated_leak)
    metrics.setdefault(
        "max_single_command_output_chars",
        event_analysis.get("max_single_command_output_chars"),
    )
    metrics.setdefault("explore_output_chars", event_analysis.get("explore_output_chars"))
    metrics.setdefault(
        "cumulative_replay_token_estimate",
        _cumulative_replay_estimate(payload, metrics, event_analysis),
    )
    metrics.setdefault(
        "first_aethyme_call_before_broad_search",
        event_analysis.get("first_aethyme_call_before_broad_search"),
    )
    metrics.setdefault(
        "broad_rg_after_successful_explore",
        event_analysis.get("broad_rg_after_successful_explore"),
    )
    metrics.setdefault(
        "successful_explore_detected",
        event_analysis.get("successful_explore_detected"),
    )
    metrics.setdefault("first_aethyme_command_index", event_analysis.get("first_aethyme_index"))
    metrics.setdefault(
        "first_broad_search_command_index", event_analysis.get("first_broad_search_index")
    )
    metrics.setdefault(
        "post_explore_broad_rg_commands",
        event_analysis.get("post_explore_broad_rg_commands", []),
    )
    metrics.setdefault("surface_flow_lane_roles", sorted(_surface_flow_lane_roles(payload)))
    metrics.setdefault("output_fingerprint", _output_fingerprint(payload))
    return metrics


def _normalize_token_metrics(metrics: dict[str, Any], payload: dict[str, Any]) -> None:
    event_usage = _event_usage_metrics(payload)
    token_estimate = _first_int_metric(metrics.get("token_estimate"), payload.get("token_estimate"))
    if token_estimate is None:
        token_estimate = _token_estimate(payload)

    total_input_tokens = _first_int_metric(
        metrics.get("total_input_tokens"),
        metrics.get("input_tokens"),
        payload.get("total_input_tokens"),
        payload.get("input_tokens"),
        event_usage.get("total_input_tokens"),
    )
    output_tokens = _first_int_metric(
        metrics.get("output_tokens"),
        payload.get("output_tokens"),
        event_usage.get("output_tokens"),
    )
    if total_input_tokens is None and output_tokens is None and token_estimate is not None:
        total_input_tokens = token_estimate
        output_tokens = 0

    cached_input_tokens = _first_int_metric(
        metrics.get("cached_input_tokens"),
        payload.get("cached_input_tokens"),
        event_usage.get("cached_input_tokens"),
    )
    if total_input_tokens is not None and cached_input_tokens is None:
        cached_input_tokens = 0
    if total_input_tokens is not None and cached_input_tokens is not None:
        cached_input_tokens = min(cached_input_tokens, total_input_tokens)

    uncached_input_tokens = _first_int_metric(
        metrics.get("uncached_input_tokens"),
        payload.get("uncached_input_tokens"),
    )
    if total_input_tokens is not None and cached_input_tokens is not None:
        uncached_input_tokens = total_input_tokens - cached_input_tokens

    uncached_plus_output_tokens = _first_int_metric(
        metrics.get("uncached_plus_output_tokens"),
        payload.get("uncached_plus_output_tokens"),
    )
    if uncached_input_tokens is not None and output_tokens is not None:
        uncached_plus_output_tokens = uncached_input_tokens + output_tokens

    if token_estimate is None and total_input_tokens is not None and output_tokens is not None:
        token_estimate = total_input_tokens + output_tokens

    if token_estimate is not None:
        metrics["token_estimate"] = token_estimate
    if total_input_tokens is not None:
        metrics["total_input_tokens"] = total_input_tokens
        metrics["input_tokens"] = total_input_tokens
    if output_tokens is not None:
        metrics["output_tokens"] = output_tokens
    if cached_input_tokens is not None:
        metrics["cached_input_tokens"] = cached_input_tokens
    if uncached_input_tokens is not None:
        metrics["uncached_input_tokens"] = uncached_input_tokens
    if uncached_plus_output_tokens is not None:
        metrics["uncached_plus_output_tokens"] = uncached_plus_output_tokens


def _first_int_metric(*values: Any) -> int | None:
    for value in values:
        if type(value) is int and value >= 0:
            return value
    return None


def _event_usage_metrics(payload: dict[str, Any]) -> dict[str, int]:
    path = _event_log_path(payload)
    if path is None:
        return {}

    usage: dict[str, int] = {}
    for event in _event_log_events(path):
        if not isinstance(event, dict) or event.get("type") != "turn.completed":
            continue
        usage_payload = event.get("usage")
        if not isinstance(usage_payload, dict):
            continue
        total_input_tokens = _first_int_metric(usage_payload.get("input_tokens"))
        output_tokens = _first_int_metric(usage_payload.get("output_tokens"))
        cached_input_tokens = _event_cached_input_tokens(usage_payload)
        if total_input_tokens is not None:
            usage["total_input_tokens"] = total_input_tokens
        if output_tokens is not None:
            usage["output_tokens"] = output_tokens
        if cached_input_tokens is not None:
            usage["cached_input_tokens"] = cached_input_tokens
    return usage


def _event_cached_input_tokens(usage_payload: dict[str, Any]) -> int | None:
    direct = _first_int_metric(
        usage_payload.get("cached_input_tokens"),
        usage_payload.get("cache_read_input_tokens"),
    )
    if direct is not None:
        return direct
    details = usage_payload.get("input_tokens_details")
    if isinstance(details, dict):
        return _first_int_metric(
            details.get("cached_tokens"),
            details.get("cache_read_tokens"),
        )
    return None


def _artifact_leak_bool(payload: dict[str, Any], key: str) -> bool:
    artifact_leakage = payload.get("artifact_leakage")
    if isinstance(artifact_leakage, dict):
        return bool(artifact_leakage.get(key))
    return False


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


def _event_log_analysis(payload: dict[str, Any]) -> dict[str, Any]:
    path = _event_log_path(payload)
    if path is None:
        command_output_chars = _int_or_zero(payload.get("command_output_chars"))
        return {
            "event_log_available": False,
            "max_single_command_output_chars": command_output_chars,
            "explore_output_chars": 0,
            "first_aethyme_index": None,
            "first_broad_search_index": None,
            "first_aethyme_call_before_broad_search": None,
            "successful_explore_detected": False,
            "broad_rg_after_successful_explore": False,
            "post_explore_broad_rg_commands": [],
        }

    max_single_output = 0
    explore_output_chars = 0
    first_aethyme_index: int | None = None
    first_broad_search_index: int | None = None
    first_successful_explore_index: int | None = None
    post_explore_broad_rg_commands: list[str] = []

    for command_index, event in enumerate(_event_log_events(path)):
        outputs = _command_output_strings(event)
        output_lengths = [len(output) for output in outputs]
        event_output_chars = sum(output_lengths)
        max_single_output = max(max_single_output, event_output_chars, *output_lengths)

        commands = _event_command_token_lists(event)
        has_aethyme_explore = any(
            _command_tokens_invoke_aethyme_explore(command) for command in commands
        )
        has_broad_search = any(_command_tokens_are_broad_search(command) for command in commands)
        has_broad_rg = any(_command_tokens_are_broad_rg(command) for command in commands)

        if has_aethyme_explore:
            if first_aethyme_index is None:
                first_aethyme_index = command_index
            explore_output_chars += event_output_chars
            if first_successful_explore_index is None and _event_looks_like_successful_explore(
                event, outputs
            ):
                first_successful_explore_index = command_index

        if has_broad_search and first_broad_search_index is None:
            first_broad_search_index = command_index

        if (
            has_broad_rg
            and first_successful_explore_index is not None
            and command_index > first_successful_explore_index
        ):
            post_explore_broad_rg_commands.extend(_command_summaries(commands))

    first_before_broad: bool | None
    if first_aethyme_index is None:
        first_before_broad = None
    elif first_broad_search_index is None:
        first_before_broad = True
    else:
        first_before_broad = first_aethyme_index < first_broad_search_index

    return {
        "event_log_available": True,
        "max_single_command_output_chars": max_single_output,
        "explore_output_chars": explore_output_chars,
        "first_aethyme_index": first_aethyme_index,
        "first_broad_search_index": first_broad_search_index,
        "first_aethyme_call_before_broad_search": first_before_broad,
        "successful_explore_detected": first_successful_explore_index is not None,
        "broad_rg_after_successful_explore": bool(post_explore_broad_rg_commands),
        "post_explore_broad_rg_commands": post_explore_broad_rg_commands[:5],
    }


def _event_log_path(payload: dict[str, Any]) -> Path | None:
    event_log_file = payload.get("event_log_file")
    if not isinstance(event_log_file, str):
        return None
    path = Path(event_log_file)
    return path if path.is_file() else None


def _event_log_events(path: Path) -> list[Any]:
    events: list[Any] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return events


def _event_command_token_lists(value: Any, *, key: str | None = None) -> list[list[Any]]:
    if isinstance(value, str):
        return [_split_command_text(value)] if key in COMMAND_FIELD_KEYS else []
    if isinstance(value, list):
        if key in COMMAND_FIELD_KEYS and any(isinstance(item, str) for item in value):
            return [value]
        commands: list[list[Any]] = []
        for item in value:
            commands.extend(_event_command_token_lists(item))
        return commands
    if isinstance(value, dict):
        commands: list[list[Any]] = []
        for item_key, item in value.items():
            commands.extend(_event_command_token_lists(item, key=str(item_key)))
        return commands
    return []


def _event_looks_like_successful_explore(event: Any, outputs: list[str]) -> bool:
    if _event_has_failed_exit_status(event):
        return False
    return bool(outputs) or _event_type_name(event).endswith(".completed")


def _event_has_failed_exit_status(value: Any) -> bool:
    if isinstance(value, dict):
        for key, item in value.items():
            key_text = str(key).lower()
            if key_text in {"exit_code", "exitcode", "returncode", "status_code"}:
                if isinstance(item, int) and item != 0:
                    return True
                if isinstance(item, str) and item not in {"0", "success", "ok", "passed"}:
                    return True
            if key_text in {"success", "ok", "passed"} and item is False:
                return True
            if _event_has_failed_exit_status(item):
                return True
    elif isinstance(value, list):
        return any(_event_has_failed_exit_status(item) for item in value)
    return False


def _event_type_name(value: Any) -> str:
    if isinstance(value, dict) and isinstance(value.get("type"), str):
        return value["type"]
    return ""


def _command_tokens_are_broad_search(value: list[Any]) -> bool:
    tokens = [item for item in value if isinstance(item, str)]
    if not tokens:
        return False
    if any(
        _command_tokens_are_broad_search(_split_command_text(payload))
        for payload in _shell_command_payloads(tokens)
    ):
        return True
    return any(_command_tail_is_broad_search(tokens, index) for index in _command_indices(tokens))


def _command_tokens_are_broad_rg(value: list[Any]) -> bool:
    tokens = [item for item in value if isinstance(item, str)]
    if not tokens:
        return False
    if any(
        _command_tokens_are_broad_rg(_split_command_text(payload))
        for payload in _shell_command_payloads(tokens)
    ):
        return True
    return any(
        Path(tokens[index]).name.lower() in {"rg", "ripgrep"}
        and _rg_command_is_broad(tokens[index:])
        for index in _command_indices(tokens)
    )


def _command_indices(tokens: list[str]) -> list[int]:
    if not tokens:
        return []
    first_name = Path(tokens[0]).name.lower()
    if first_name == "env":
        for index, token in enumerate(tokens[1:], start=1):
            if token.startswith("-") or _looks_like_env_assignment(token):
                continue
            return [index]
    return [0]


def _looks_like_env_assignment(token: str) -> bool:
    name, separator, _value = token.partition("=")
    return bool(separator and name and re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", name))


def _command_tail_is_broad_search(tokens: list[str], index: int) -> bool:
    name = Path(tokens[index]).name.lower()
    tail = tokens[index:]
    if name in {"rg", "ripgrep"}:
        return _rg_command_is_broad(tail)
    if name == "grep":
        return _grep_command_is_broad(tail)
    if name == "find":
        return _find_command_is_broad(tail)
    if name == "git" and index + 1 < len(tokens) and tokens[index + 1] == "grep":
        return _grep_command_is_broad(tokens[index + 1 :])
    return False


def _rg_command_is_broad(tokens: list[str]) -> bool:
    operands = _search_command_operands(tokens[1:])
    if any(token == "--files" for token in tokens[1:]):
        path_operands = operands
    else:
        path_operands = operands[1:] if operands else []
    if not path_operands:
        return True
    return any(_search_path_is_broad(path) for path in path_operands)


def _grep_command_is_broad(tokens: list[str]) -> bool:
    recursive = any(token in {"-R", "-r", "--recursive"} for token in tokens[1:])
    if not recursive:
        return False
    operands = _search_command_operands(tokens[1:])
    path_operands = operands[1:] if operands else []
    if not path_operands:
        return True
    return any(_search_path_is_broad(path) for path in path_operands)


def _find_command_is_broad(tokens: list[str]) -> bool:
    operands = _search_command_operands(tokens[1:])
    if not operands:
        return True
    return any(_search_path_is_broad(path) for path in operands[:2])


def _search_command_operands(tokens: list[str]) -> list[str]:
    operands: list[str] = []
    skip_next = False
    flags_with_value = {
        "-A",
        "-B",
        "-C",
        "-e",
        "-g",
        "-m",
        "-t",
        "-T",
        "--after-context",
        "--before-context",
        "--context",
        "--glob",
        "--ignore-file",
        "--max-count",
        "--regexp",
        "--sort",
        "--type",
        "--type-not",
    }
    for index, token in enumerate(tokens):
        if skip_next:
            skip_next = False
            continue
        if token == "--":
            operands.extend(tokens[index + 1 :])
            break
        if token in flags_with_value:
            skip_next = True
            continue
        if any(token.startswith(f"{flag}=") for flag in flags_with_value if flag.startswith("--")):
            continue
        if token.startswith("-"):
            continue
        operands.append(token)
    return operands


def _search_path_is_broad(path: str) -> bool:
    normalized = path.strip().strip("'\"").rstrip("/")
    if not normalized:
        return False
    lower = normalized.lower()
    if lower in {"", ".", "./", "$pwd", "${pwd}", "$repo", "$repo_path"}:
        return True
    if any(char in normalized for char in "*?[]"):
        return True
    if lower in {
        "app",
        "apps",
        "backend",
        "client",
        "frontend",
        "lib",
        "package",
        "packages",
        "server",
        "src",
        "test",
        "tests",
        "web",
    }:
        return True
    return Path(normalized).suffix == "" and "/" in normalized


def _command_summaries(commands: list[list[Any]]) -> list[str]:
    return [_command_summary(command) for command in commands if command]


def _command_summary(command: list[Any]) -> str:
    text = " ".join(shlex.quote(str(token)) for token in command)
    return text[:240]


def _cumulative_replay_estimate(
    payload: dict[str, Any],
    metrics: dict[str, Any],
    event_analysis: dict[str, Any],
) -> int:
    existing = metrics.get("cumulative_replay_token_estimate")
    if isinstance(existing, int) and existing >= 0:
        return existing

    event_log_chars = _payload_or_file_chars(payload, "event_log_chars", "event_log_file")
    stderr_chars = _payload_or_file_chars(payload, "stderr_chars", "stderr_file")
    if event_log_chars == 0:
        event_log_chars = _metric_int(metrics, "command_output_chars")

    answer_chars = 0
    final_output = payload.get("final_output_message")
    if isinstance(final_output, str):
        answer_chars += len(final_output)
    structured_output = payload.get("structured_output")
    if structured_output is not None:
        answer_chars += len(json.dumps(structured_output, sort_keys=True, ensure_ascii=True))

    total_chars = event_log_chars + stderr_chars + answer_chars
    if total_chars == 0:
        return _metric_int(metrics, "token_estimate")
    return (total_chars + 3) // 4


def _payload_or_file_chars(payload: dict[str, Any], count_key: str, file_key: str) -> int:
    count = payload.get(count_key)
    if isinstance(count, int) and count >= 0:
        return count
    path_value = payload.get(file_key)
    if not isinstance(path_value, str):
        return 0
    path = Path(path_value)
    if not path.is_file():
        return 0
    try:
        return path.stat().st_size
    except OSError:
        return 0


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
    shell_invocation = any(
        _command_tokens_invoke_aethyme_explore(_split_command_text(payload))
        for payload in _shell_command_payloads(tokens)
    )
    if shell_invocation:
        return True
    return has_aethyme_binary and has_explore_subcommand


def _is_aethyme_binary(token: str) -> bool:
    normalized = token.strip("\"'")
    if normalized in {"$AETHYME_BIN", "${AETHYME_BIN}"}:
        return True
    return Path(token).name.lower() in {"aethyme", "aethyme-engine-cli"}


def _shell_command_payloads(tokens: list[str]) -> list[str]:
    payloads: list[str] = []
    for index, token in enumerate(tokens[:-1]):
        if _looks_like_shell_c_flag(token) and index > 0 and _is_shell_binary(tokens[index - 1]):
            payloads.append(tokens[index + 1])
    return payloads


def _looks_like_shell_c_flag(token: str) -> bool:
    return token.startswith("-") and "c" in token[1:]


def _is_shell_binary(token: str) -> bool:
    return Path(token).name.lower() in {"bash", "dash", "ksh", "sh", "zsh"}


def _split_values(value: Any) -> list[str]:
    if value is None:
        return []
    raw_values = value if isinstance(value, list) else [value]
    parsed: list[str] = []
    for raw_value in raw_values:
        if not isinstance(raw_value, str):
            continue
        for part in raw_value.split(","):
            normalized = _normalize_fixture_id(part)
            if normalized:
                parsed.append(normalized)
    return parsed


def _resolve_pair_fixture_id(
    control: dict[str, Any],
    aethyme: dict[str, Any],
    override: str | None,
) -> str | None:
    fixture_ids = []
    if override:
        fixture_ids.append(_normalize_fixture_id(override))
    for payload in (control, aethyme):
        payload_fixture = _fixture_id_from_payload(payload)
        if payload_fixture:
            fixture_ids.append(payload_fixture)
    unique = sorted(set(fixture_ids))
    if not unique:
        return None
    if len(unique) > 1:
        return "__mismatch__:" + ",".join(unique)
    return unique[0]


def _fixture_id_from_payload(payload: dict[str, Any]) -> str | None:
    for value in (
        payload.get("fixture_id"),
        payload.get("fixture"),
        payload.get("task_class"),
    ):
        if isinstance(value, str) and value.strip():
            normalized = _normalize_fixture_id(value)
            if normalized in REQUIRED_PLAYGROUND_FIXTURES:
                return normalized
    contract = payload.get("contract")
    if isinstance(contract, dict):
        for value in (
            contract.get("fixture_id"),
            contract.get("fixture"),
            contract.get("task_class"),
        ):
            if isinstance(value, str) and value.strip():
                normalized = _normalize_fixture_id(value)
                if normalized in REQUIRED_PLAYGROUND_FIXTURES:
                    return normalized
    fixture = payload.get("fixture")
    if isinstance(fixture, dict):
        value = fixture.get("id") or fixture.get("fixture_id")
        if isinstance(value, str) and value.strip():
            normalized = _normalize_fixture_id(value)
            if normalized in REQUIRED_PLAYGROUND_FIXTURES:
                return normalized
    return None


def _fixture_check(fixture_id: str | None) -> dict[str, Any]:
    if fixture_id is None:
        return {
            "name": "playground_fixture_declared",
            "passed": True,
            "skipped": True,
            "fixture_id": None,
            "failure": None,
        }
    if fixture_id.startswith("__mismatch__:"):
        return {
            "name": "playground_fixture_declared",
            "passed": False,
            "fixture_id": fixture_id,
            "failure": f"control/aethyme fixture ids disagree: {fixture_id.removeprefix('__mismatch__:')}",
        }
    passed = fixture_id in REQUIRED_PLAYGROUND_FIXTURES
    return {
        "name": "playground_fixture_declared",
        "passed": passed,
        "fixture_id": fixture_id,
        "fixture_name": REQUIRED_PLAYGROUND_FIXTURES.get(fixture_id),
        "failure": None if passed else f"unknown required fixture id: {fixture_id}",
    }


def _normalize_fixture_id(value: str) -> str:
    return re.sub(r"_+", "_", re.sub(r"[^a-z0-9]+", "_", value.lower())).strip("_")


def _expected_missing_coverage(payload: dict[str, Any]) -> list[str]:
    for value in (
        payload.get("expected_missing_coverage"),
        payload.get("expected_missing_surfaces"),
    ):
        parsed = _split_values(value)
        if parsed:
            return parsed
    fixture = payload.get("fixture")
    if isinstance(fixture, dict):
        parsed = _split_values(fixture.get("expected_missing_coverage"))
        if parsed:
            return parsed
    return []


def _playground_contract_checks(
    label: str,
    payload: dict[str, Any],
    *,
    require: bool,
) -> list[dict[str, Any]]:
    contract = payload.get("contract")
    if not isinstance(contract, dict):
        return [
            {
                "name": f"{label}_playground_contract_present",
                "passed": not require,
                "skipped": not require,
                "failure": None if not require else f"{label} result is missing contract metadata",
            }
        ]

    repo_path = _payload_repo_path(payload)
    playground_repo_passed = (
        contract.get("playground_repo") is True
        if require
        else contract.get("playground_repo") is not False
    )
    not_self_eval_passed = (
        contract.get("aethyme_self_eval") is False
        if require
        else contract.get("aethyme_self_eval") is not True
    )
    checks = [
        _boolean_check(
            f"{label}_playground_repo",
            playground_repo_passed,
            f"{label} result was not marked as a Playground repo",
        ),
        _boolean_check(
            f"{label}_not_aethyme_self_eval",
            not_self_eval_passed,
            f"{label} result was marked as an Aethyme self-eval",
        ),
    ]
    if repo_path is not None:
        checks.append(
            _boolean_check(
                f"{label}_repo_path_not_aethyme_checkout",
                not _is_aethyme_checkout_path(repo_path),
                f"{label} repo path points inside the Aethyme checkout",
            )
        )
    elif require:
        checks.append(
            {
                "name": f"{label}_repo_path_reported",
                "passed": False,
                "failure": f"{label} result is missing repo_path contract metadata",
            }
        )
    return checks


def _payload_repo_path(payload: dict[str, Any]) -> Path | None:
    for value in (payload.get("repo_path"), payload.get("repository_path")):
        if isinstance(value, str) and value:
            return Path(value).expanduser()
    contract = payload.get("contract")
    if isinstance(contract, dict):
        value = contract.get("repo_path") or contract.get("repository_path")
        if isinstance(value, str) and value:
            return Path(value).expanduser()
    return None


def _is_aethyme_checkout_path(path: Path) -> bool:
    package_root = Path(__file__).resolve().parents[2]
    monorepo_root = package_root.parents[1]
    return _is_relative_to(path, monorepo_root) or _is_relative_to(path, package_root)


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.resolve().relative_to(root.resolve())
        return True
    except (OSError, ValueError):
        return False


def _command_output_bound_checks(
    control_metrics: dict[str, Any],
    aethyme_metrics: dict[str, Any],
    max_command_output_chars: int | None,
) -> list[dict[str, Any]]:
    if max_command_output_chars is None:
        return []
    return [
        _command_output_bound_check("control", control_metrics, max_command_output_chars),
        _command_output_bound_check("aethyme", aethyme_metrics, max_command_output_chars),
    ]


def _command_output_bound_check(
    label: str,
    metrics: dict[str, Any],
    max_command_output_chars: int,
) -> dict[str, Any]:
    actual = _metric_int(metrics, "command_output_chars")
    passed = actual <= max_command_output_chars
    return {
        "name": f"{label}_command_output_bounded",
        "passed": passed,
        "actual": actual,
        "limit": max_command_output_chars,
        "failure": None if passed else f"{label} command output exceeded absolute cap",
    }


def _single_command_output_bound_checks(
    control_metrics: dict[str, Any],
    aethyme_metrics: dict[str, Any],
    max_single_command_output_chars: int | None,
) -> list[dict[str, Any]]:
    if max_single_command_output_chars is None:
        return []
    return [
        _single_command_output_bound_check(
            "control",
            control_metrics,
            max_single_command_output_chars,
        ),
        _single_command_output_bound_check(
            "aethyme",
            aethyme_metrics,
            max_single_command_output_chars,
        ),
    ]


def _single_command_output_bound_check(
    label: str,
    metrics: dict[str, Any],
    max_single_command_output_chars: int,
) -> dict[str, Any]:
    actual = _metric_int(metrics, "max_single_command_output_chars")
    passed = actual <= max_single_command_output_chars
    return {
        "name": f"{label}_single_command_output_bounded",
        "passed": passed,
        "actual": actual,
        "limit": max_single_command_output_chars,
        "failure": None if passed else f"{label} single command output exceeded cap",
    }


def _explore_output_bound_check(
    aethyme_metrics: dict[str, Any],
    max_explore_output_chars: int | None,
) -> dict[str, Any]:
    if max_explore_output_chars is None:
        return {
            "name": "aethyme_explore_output_bounded",
            "passed": True,
            "skipped": True,
            "failure": None,
        }
    actual = _metric_int(aethyme_metrics, "explore_output_chars")
    passed = actual <= max_explore_output_chars
    return {
        "name": "aethyme_explore_output_bounded",
        "passed": passed,
        "actual": actual,
        "limit": max_explore_output_chars,
        "failure": None if passed else "Aethyme Explore output exceeded cap",
    }


def _cumulative_replay_bound_checks(
    control_metrics: dict[str, Any],
    aethyme_metrics: dict[str, Any],
    max_cumulative_replay_estimate: int | None,
) -> list[dict[str, Any]]:
    if max_cumulative_replay_estimate is None:
        return []
    return [
        _cumulative_replay_bound_check(
            "control",
            control_metrics,
            max_cumulative_replay_estimate,
        ),
        _cumulative_replay_bound_check(
            "aethyme",
            aethyme_metrics,
            max_cumulative_replay_estimate,
        ),
    ]


def _cumulative_replay_bound_check(
    label: str,
    metrics: dict[str, Any],
    max_cumulative_replay_estimate: int,
) -> dict[str, Any]:
    actual = _metric_int(metrics, "cumulative_replay_token_estimate")
    passed = actual <= max_cumulative_replay_estimate
    return {
        "name": f"{label}_cumulative_replay_estimate_bounded",
        "passed": passed,
        "actual": actual,
        "limit": max_cumulative_replay_estimate,
        "failure": None if passed else f"{label} replay estimate exceeded cap",
    }


def _first_aethyme_before_broad_search_check(
    aethyme_metrics: dict[str, Any],
    *,
    require: bool,
) -> dict[str, Any]:
    value = aethyme_metrics.get("first_aethyme_call_before_broad_search")
    if not isinstance(value, bool):
        return {
            "name": "aethyme_first_call_before_broad_search",
            "passed": not require,
            "skipped": not require,
            "value": value,
            "failure": None
            if not require
            else "missing event-log evidence for Aethyme/broad-search ordering",
        }
    return {
        "name": "aethyme_first_call_before_broad_search",
        "passed": value,
        "first_aethyme_command_index": aethyme_metrics.get("first_aethyme_command_index"),
        "first_broad_search_command_index": aethyme_metrics.get("first_broad_search_command_index"),
        "failure": None if value else "first broad search happened before Aethyme Explore",
    }


def _broad_rg_after_successful_explore_warning(aethyme_metrics: dict[str, Any]) -> dict[str, Any]:
    warned = bool(aethyme_metrics.get("broad_rg_after_successful_explore"))
    return {
        "name": "broad_rg_after_successful_explore",
        "passed": True,
        "warning": warned,
        "commands": aethyme_metrics.get("post_explore_broad_rg_commands", []),
        "failure": None,
    }


def _determinism_checks(
    control: dict[str, Any],
    aethyme: dict[str, Any],
    control_repeat: dict[str, Any] | None,
    aethyme_repeat: dict[str, Any] | None,
    *,
    require: bool,
) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    for label, payload, repeat in (
        ("control", control, control_repeat),
        ("aethyme", aethyme, aethyme_repeat),
    ):
        if repeat is None and not require:
            continue
        checks.append(_determinism_check(label, payload, repeat, require=require))
    return checks


def _determinism_check(
    label: str,
    payload: dict[str, Any],
    repeat: dict[str, Any] | None,
    *,
    require: bool,
) -> dict[str, Any]:
    if repeat is None:
        return {
            "name": f"{label}_output_deterministic",
            "passed": not require,
            "skipped": not require,
            "failure": None if not require else f"{label} repeat result is required",
        }
    first = _output_fingerprint(payload)
    second = _output_fingerprint(repeat)
    passed = first == second
    return {
        "name": f"{label}_output_deterministic",
        "passed": passed,
        "fingerprint": first,
        "repeat_fingerprint": second,
        "failure": None if passed else f"{label} output changed across repeat runs",
    }


def _output_fingerprint(payload: dict[str, Any]) -> str:
    encoded = json.dumps(
        _deterministic_output_surface(payload),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _deterministic_output_surface(payload: dict[str, Any]) -> Any:
    structured_output = payload.get("structured_output")
    if structured_output is not None:
        return structured_output
    if payload.get("final_output_message") is not None:
        return payload.get("final_output_message")
    return {
        key: value for key, value in payload.items() if key not in NONDETERMINISTIC_OUTPUT_FIELDS
    }


def _coverage_report_check(
    aethyme: dict[str, Any],
    *,
    expected_missing_coverage: list[str],
    require: bool,
) -> dict[str, Any]:
    should_require = require or bool(expected_missing_coverage)
    candidates = _observability_candidates(aethyme)
    if not candidates:
        return {
            "name": "surface_flow_coverage_reported",
            "passed": not should_require,
            "skipped": not should_require,
            "expected_missing_coverage": expected_missing_coverage,
            "failure": None
            if not should_require
            else "Aethyme result did not expose Surface/Flow observability",
        }

    best = _assess_coverage_candidate(candidates[0], expected_missing_coverage)
    for candidate in candidates:
        assessment = _assess_coverage_candidate(candidate, expected_missing_coverage)
        if assessment["passed"]:
            return assessment
        best = assessment
    return best


def _surface_flow_lanes_check(
    aethyme: dict[str, Any],
    aethyme_metrics: dict[str, Any],
    *,
    fixture_id: str | None,
    require: bool,
) -> dict[str, Any]:
    if not require or not _is_auth_token_task(aethyme, fixture_id):
        return {
            "name": "auth_token_surface_flow_lanes_present",
            "passed": True,
            "skipped": True,
            "fixture_id": fixture_id,
            "failure": None,
        }

    roles = _surface_flow_lane_roles_from_metrics(aethyme_metrics)
    roles.update(_surface_flow_lane_roles(aethyme))
    required_roles = _required_auth_surface_lane_roles(fixture_id)
    if required_roles:
        missing_roles = sorted(required_roles - roles)
        passed = not missing_roles
        failure = None if passed else f"missing required Surface/Flow lanes: {missing_roles}"
    else:
        matching_roles = sorted(roles & AUTH_SURFACE_LANE_ROLES)
        passed = bool(matching_roles)
        failure = None if passed else "auth/token task did not expose Surface/Flow subsystem lanes"
        missing_roles = []

    return {
        "name": "auth_token_surface_flow_lanes_present",
        "passed": passed,
        "fixture_id": fixture_id,
        "roles": sorted(roles),
        "required_roles": sorted(required_roles),
        "missing_roles": missing_roles,
        "failure": failure,
    }


def _is_auth_token_task(payload: dict[str, Any], fixture_id: str | None) -> bool:
    if fixture_id in AUTH_TOKEN_FIXTURE_IDS:
        return True
    for value in _payload_task_descriptors(payload):
        lower = value.lower()
        if any(keyword in lower for keyword in AUTH_TOKEN_TASK_KEYWORDS):
            return True
    return False


def _payload_task_descriptors(payload: dict[str, Any]) -> list[str]:
    descriptors: list[str] = []
    for key in ("fixture_id", "fixture", "task_class", "request", "prompt"):
        value = payload.get(key)
        if isinstance(value, str):
            descriptors.append(value)
    contract = payload.get("contract")
    if isinstance(contract, dict):
        for key in ("fixture_id", "fixture", "task_class", "request", "prompt"):
            value = contract.get(key)
            if isinstance(value, str):
                descriptors.append(value)
    fixture = payload.get("fixture")
    if isinstance(fixture, dict):
        for key in ("id", "fixture_id", "task_class", "request", "prompt"):
            value = fixture.get(key)
            if isinstance(value, str):
                descriptors.append(value)
    return descriptors


def _required_auth_surface_lane_roles(fixture_id: str | None) -> set[str]:
    if fixture_id is None:
        return set()
    return set(AUTH_FIXTURE_REQUIRED_LANES.get(fixture_id, set()))


def _surface_flow_lane_roles_from_metrics(metrics: dict[str, Any]) -> set[str]:
    value = metrics.get("surface_flow_lane_roles")
    if not isinstance(value, list):
        return set()
    return {_canonical_surface_flow_lane_role(item) for item in value if isinstance(item, str)}


def _surface_flow_lane_roles(payload: dict[str, Any]) -> set[str]:
    roles: set[str] = set()
    _collect_surface_flow_lane_roles(payload.get("structured_output"), roles)
    _collect_surface_flow_lane_roles(payload.get("final_output_message"), roles)
    event_log_file = payload.get("event_log_file")
    if isinstance(event_log_file, str) and Path(event_log_file).is_file():
        for line in Path(event_log_file).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            for output_text in _command_output_strings(event):
                for decoded in _decoded_json_values(output_text):
                    _collect_surface_flow_lane_roles(decoded, roles)
    return roles


def _collect_surface_flow_lane_roles(value: Any, roles: set[str]) -> None:
    if isinstance(value, str):
        for decoded in _decoded_json_values(value):
            _collect_surface_flow_lane_roles(decoded, roles)
        return
    if isinstance(value, list):
        for item in value:
            _collect_surface_flow_lane_roles(item, roles)
        return
    if not isinstance(value, dict):
        return

    subsystems = value.get("subsystems")
    if isinstance(subsystems, list):
        for subsystem in subsystems:
            if isinstance(subsystem, dict) and isinstance(subsystem.get("role"), str):
                roles.add(_canonical_surface_flow_lane_role(subsystem["role"]))
    for item in value.values():
        _collect_surface_flow_lane_roles(item, roles)


def _canonical_surface_flow_lane_role(value: str) -> str:
    normalized = _normalize_fixture_id(value)
    return AUTH_SURFACE_LANE_ROLE_ALIASES.get(normalized, normalized)


def _assess_coverage_candidate(
    observability: dict[str, Any],
    expected_missing_coverage: list[str],
) -> dict[str, Any]:
    surface_flow = observability.get("surface_flow_graph")
    if not isinstance(surface_flow, dict):
        surface_flow = observability
    coverage = surface_flow.get("coverage")
    missing = surface_flow.get("missing_expected_surfaces")
    if not isinstance(missing, list) and isinstance(
        observability.get("missing_expected_surfaces"), list
    ):
        missing = observability.get("missing_expected_surfaces")

    has_shape = isinstance(coverage, dict) and isinstance(missing, list)
    observed_missing = _missing_surface_types(missing if isinstance(missing, list) else [])
    hidden_missing = (
        _coverage_missing_from_statuses(coverage) - observed_missing
        if isinstance(coverage, dict)
        else set()
    )
    expected_missing = set(expected_missing_coverage)
    missing_expected = expected_missing - observed_missing
    passed = has_shape and not hidden_missing and not missing_expected
    failure = None
    if not has_shape:
        failure = "Surface/Flow observability is missing coverage or missing_expected_surfaces"
    elif hidden_missing:
        failure = f"coverage statuses imply hidden missing surfaces: {sorted(hidden_missing)}"
    elif missing_expected:
        failure = f"expected missing coverage was not reported: {sorted(missing_expected)}"
    return {
        "name": "surface_flow_coverage_reported",
        "passed": passed,
        "observed_missing_coverage": sorted(observed_missing),
        "expected_missing_coverage": sorted(expected_missing),
        "hidden_missing_coverage": sorted(hidden_missing),
        "failure": failure,
    }


def _observability_candidates(payload: dict[str, Any]) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    _collect_observability_candidates(payload, candidates)
    event_log_file = payload.get("event_log_file")
    if isinstance(event_log_file, str) and Path(event_log_file).is_file():
        for line in Path(event_log_file).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            for output_text in _command_output_strings(event):
                for decoded in _decoded_json_values(output_text):
                    _collect_observability_candidates(decoded, candidates)
    return candidates


def _collect_observability_candidates(value: Any, candidates: list[dict[str, Any]]) -> None:
    if isinstance(value, dict):
        observability = value.get("observability")
        if isinstance(observability, dict):
            candidates.append(observability)
        if "surface_flow_graph" in value or (
            "coverage" in value and "missing_expected_surfaces" in value
        ):
            candidates.append(value)
        for item in value.values():
            _collect_observability_candidates(item, candidates)
    elif isinstance(value, list):
        for item in value:
            _collect_observability_candidates(item, candidates)


def _command_output_strings(value: Any, *, key: str | None = None) -> list[str]:
    if isinstance(value, str):
        return [value] if key in COMMAND_OUTPUT_KEYS else []
    if isinstance(value, list):
        strings: list[str] = []
        for item in value:
            strings.extend(_command_output_strings(item))
        return strings
    if isinstance(value, dict):
        strings = []
        for item_key, item in value.items():
            strings.extend(_command_output_strings(item, key=str(item_key)))
        return strings
    return []


def _decoded_json_values(value: str) -> list[Any]:
    decoded: list[Any] = []
    decoder = json.JSONDecoder()
    index = 0
    while index < len(value):
        next_object = len(value)
        for marker in ("{", "["):
            marker_index = value.find(marker, index)
            if marker_index != -1:
                next_object = min(next_object, marker_index)
        if next_object == len(value):
            break
        try:
            parsed, end = decoder.raw_decode(value, next_object)
        except json.JSONDecodeError:
            index = next_object + 1
            continue
        decoded.append(parsed)
        index = max(end, next_object + 1)
    return decoded


def _missing_surface_types(missing: list[Any]) -> set[str]:
    surface_types = set()
    for item in missing:
        if isinstance(item, str):
            normalized = _normalize_fixture_id(item)
        elif isinstance(item, dict) and isinstance(item.get("surface_type"), str):
            normalized = _normalize_fixture_id(item["surface_type"])
        else:
            normalized = ""
        if normalized:
            surface_types.add(normalized)
    return surface_types


def _coverage_missing_from_statuses(coverage: dict[str, Any]) -> set[str]:
    missing_statuses = {"partially_indexed", "source_present_not_indexed"}
    missing = set()
    for surface_type, value in coverage.items():
        if not isinstance(value, dict):
            continue
        if value.get("status") in missing_statuses:
            missing.add(_normalize_fixture_id(str(surface_type)))
    return missing


def _boolean_check(name: str, passed: bool, failure: str) -> dict[str, Any]:
    return {
        "name": name,
        "passed": passed,
        "failure": None if passed else failure,
    }


def _token_estimate_delta_observation(
    control_metrics: dict[str, Any],
    aethyme_metrics: dict[str, Any],
    max_delta_ratio: float,
    slack: int,
) -> dict[str, Any]:
    baseline = _metric_int(control_metrics, "token_estimate")
    actual = _metric_int(aethyme_metrics, "token_estimate")
    limit = int(baseline * (1.0 + max_delta_ratio)) + slack
    return {
        "name": "token_estimate_delta",
        "passed": True,
        "warning": actual > limit,
        "baseline": baseline,
        "actual": actual,
        "delta": actual - baseline,
        "limit": limit,
        "max_delta_ratio": max_delta_ratio,
        "slack": slack,
        "budget_role": "context_pressure_telemetry",
        "strict_budget_metric": "uncached_plus_output_tokens",
        "failure": None,
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
