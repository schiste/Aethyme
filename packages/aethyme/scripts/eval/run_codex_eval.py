#!/usr/bin/env python3
"""Run a single Aethyme eval prompt through Codex CLI and emit runner JSON."""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

GENERATED_ARTIFACTS = (
    ".codex",
    ".aethyme",
    ".chau7",
    ".claude",
    "AGENTS.md",
    "CLAUDE.md",
)
COMMAND_OUTPUT_KEYS = {"aggregated_output", "output", "stdout", "stderr"}
COMMAND_FIELD_KEYS = {"cmd", "command"}
PATH_LEAK_MARKERS = (".aethyme",)
MAX_REPORTED_LEAKS = 20
CODEX_EVAL_ENGINE_SOCKET_DIR = Path("/tmp/aethyme-codex-engine-sockets")
_PATH_LEAK_PATTERNS = {
    ".aethyme": re.compile(r"(?:^|[\s'\"`({\[<:=!,/])(?:\./)?\.aethyme(?:/|$)")
}


def main() -> int:
    prompt = os.environ.get("AETHYME_EVAL_PROMPT", "")
    repo_path = Path(os.environ["AETHYME_EVAL_REPO"]).expanduser().resolve()
    schema_file, schema_cleanup_dir = _resolve_schema_file()
    tool_repo = Path(
        os.environ.get("AETHYME_EVAL_TOOL_REPO", Path(__file__).resolve().parents[2])
    ).resolve()
    arm = os.environ.get("AETHYME_EVAL_ARM", "")

    try:
        arm = _resolve_eval_arm()
        contract = _enforce_eval_contract(repo_path, tool_repo, arm)
        temp_root = _resolve_artifact_dir(arm)
        events_file = temp_root / "events.jsonl"
        stderr_file = temp_root / "stderr.log"
        last_message_file = temp_root / "last-message.json"
        command = _build_codex_command(
            repo_path=repo_path,
            tool_repo=tool_repo,
            arm=arm,
            schema_file=schema_file,
            last_message_file=last_message_file,
            prompt=prompt,
        )

        started_at = time.monotonic()
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            env=_codex_env(arm, tool_repo),
        )
        wall_time_seconds = time.monotonic() - started_at

        events_file.write_text(result.stdout, encoding="utf-8")
        stderr_file.write_text(result.stderr, encoding="utf-8")
        (temp_root / "command.json").write_text(
            json.dumps(_redact_prompt(command), indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        (temp_root / "contract.json").write_text(
            json.dumps(contract, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        structured_output, final_output_message = _read_last_message(last_message_file)
        usage = _parse_usage(events_file)
        error_message = _last_error(events_file)
        command_output_chars = _command_output_chars(events_file)
        artifact_leakage = _detect_artifact_leakage(
            structured_output=structured_output,
            final_output_message=final_output_message or error_message,
            events_file=events_file,
        )
        (temp_root / "leakage.json").write_text(
            json.dumps(artifact_leakage, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        payload = {
            "arm": arm,
            "artifact_dir": str(temp_root),
            "event_log_file": str(events_file),
            "stderr_file": str(stderr_file),
            "last_message_file": str(last_message_file),
            "leakage_file": str(temp_root / "leakage.json"),
            "wall_time_seconds": round(wall_time_seconds, 3),
            "command_output_chars": command_output_chars,
            "event_log_chars": len(result.stdout),
            "stderr_chars": len(result.stderr),
            "input_tokens": usage.get("input_tokens"),
            "output_tokens": usage.get("output_tokens"),
            "retries": usage.get("retries"),
            "review_burden": None,
            "final_output_message": final_output_message or error_message,
            "structured_output": structured_output,
            "runner_settings": {
                "codex_exec": True,
                "ignore_user_config": True,
                "sandbox": "workspace-write",
                "json_events": True,
                "added_tool_repo": arm == "aethyme",
                "tool_repo": str(tool_repo) if arm == "aethyme" else None,
            },
            "contract": contract,
            "artifact_leakage": artifact_leakage,
        }
        payload["regression_metrics"] = _regression_metrics(
            arm=arm,
            structured_output=structured_output,
            usage=usage,
            command_output_chars=command_output_chars,
            event_log_chars=len(result.stdout),
            artifact_leakage=artifact_leakage,
            events_file=events_file,
        )
        print(json.dumps(payload))
        if result.stderr:
            sys.stderr.write(result.stderr)
        if result.returncode != 0 and error_message:
            sys.stderr.write(f"\nCodex error: {error_message}\n")
        if artifact_leakage["aethyme_path_leaked"]:
            sys.stderr.write(
                "\nGenerated artifact leakage detected: .aethyme path appeared in "
                "structured output, command output, or final answer.\n"
            )
            return 3
        return result.returncode
    except ContractError as exc:
        print(json.dumps({"error": str(exc), "contract_failed": True, "arm": arm}))
        return 2
    finally:
        if schema_cleanup_dir is not None:
            shutil.rmtree(schema_cleanup_dir, ignore_errors=True)


class ContractError(RuntimeError):
    """The playground A/B contract was violated before Codex was launched."""


def _resolve_eval_arm() -> str:
    arm = os.environ.get("AETHYME_EVAL_ARM")
    if arm not in {"control", "aethyme"}:
        raise ContractError("AETHYME_EVAL_ARM must be 'control' or 'aethyme'")
    return arm


def _build_codex_command(
    *,
    repo_path: Path,
    tool_repo: Path,
    arm: str,
    schema_file: Path,
    last_message_file: Path,
    prompt: str,
) -> list[str]:
    command = [
        "codex",
        "exec",
        "--ignore-user-config",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "--color",
        "never",
        "--json",
        "--output-schema",
        str(schema_file),
        "--output-last-message",
        str(last_message_file),
        "--add-dir",
        "/tmp",
    ]
    if arm == "aethyme":
        command.extend(["--add-dir", str(tool_repo)])
    command.extend(["-C", str(repo_path), prompt])
    return command


def _codex_env(arm: str, tool_repo: Path) -> dict[str, str]:
    env = os.environ.copy()
    if arm == "control":
        for key in list(env):
            if key.startswith(("AETHYME", "AETHYMEBENCH")):
                env.pop(key, None)
        return env
    env["AETHYME_ROOT"] = str(tool_repo)
    env["AETHYME_ENGINE_SOCKET_DIR"] = str(CODEX_EVAL_ENGINE_SOCKET_DIR)
    return env


def _resolve_artifact_dir(arm: str) -> Path:
    root = os.environ.get("AETHYME_EVAL_ARTIFACT_DIR")
    if root:
        artifact_dir = Path(root).expanduser().resolve()
        artifact_dir.mkdir(parents=True, exist_ok=True)
        return artifact_dir
    return Path(tempfile.mkdtemp(prefix=f"aethyme-codex-{arm}-eval-")).resolve()


def _enforce_eval_contract(repo_path: Path, tool_repo: Path, arm: str) -> dict[str, Any]:
    _assert_playground_repo(repo_path, tool_repo)
    contract: dict[str, Any] = {
        "playground_repo": True,
        "aethyme_self_eval": False,
        "arm": arm,
        "repo_path": str(repo_path),
    }
    if arm == "control":
        _assert_control_repo_clean(repo_path)
        contract.update(
            {
                "control_no_generated_artifacts": True,
                "control_tool_repo_added": False,
            }
        )
    else:
        _assert_aethyme_repo_surface(repo_path)
        contract.update(
            {
                "aethyme_intended_surface_present": True,
                "aethyme_internal_eval_skill_absent": True,
            }
        )
    return contract


def _assert_playground_repo(repo_path: Path, tool_repo: Path) -> None:
    package_root = Path(__file__).resolve().parents[2]
    monorepo_root = package_root.parents[1]
    if _is_relative_to(repo_path, monorepo_root) or _is_relative_to(repo_path, tool_repo):
        raise ContractError("Eval target must be a Playground repo, never Aethyme itself")

    roots = _playground_roots()
    if not any(_is_relative_to(repo_path, root) for root in roots):
        formatted = ", ".join(str(root) for root in roots)
        raise ContractError(f"Eval target must live under a Playground root ({formatted})")


def _playground_roots() -> tuple[Path, ...]:
    raw_roots = os.environ.get("AETHYME_PLAYGROUND_ROOTS")
    if raw_roots:
        roots = [
            Path(value).expanduser().resolve() for value in raw_roots.split(os.pathsep) if value
        ]
    else:
        home = Path.home()
        roots = [
            home / "Repositories" / "Playground",
            home / "Downloads" / "Repositories" / "Playground",
        ]
        if os.environ.get("AETHYME_PLAYGROUND_ROOT"):
            roots.insert(0, Path(os.environ["AETHYME_PLAYGROUND_ROOT"]).expanduser().resolve())
    return tuple(dict.fromkeys(root.resolve() for root in roots))


def _assert_control_repo_clean(repo_path: Path) -> None:
    leaked = [path for path in GENERATED_ARTIFACTS if (repo_path / path).exists()]
    if leaked:
        raise ContractError(f"Control repo contains generated/tool artifacts: {', '.join(leaked)}")


def _assert_aethyme_repo_surface(repo_path: Path) -> None:
    required = (
        ".codex/skills/aethyme/SKILL.md",
        ".codex/skills/aethyme/references/explore.md",
        ".aethyme/graph",
        ".aethyme/graph_store.redb",
        "AGENTS.md",
        "CLAUDE.md",
    )
    missing = [path for path in required if not (repo_path / path).exists()]
    if missing:
        raise ContractError(
            f"Aethyme arm missing intended enhancement surface: {', '.join(missing)}"
        )
    if (repo_path / ".codex/skills/eval").exists():
        raise ContractError("Aethyme arm contains internal eval skill leakage")


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.resolve().relative_to(root.resolve())
        return True
    except ValueError:
        return False


def _redact_prompt(command: list[str]) -> list[str]:
    if not command:
        return command
    redacted = list(command)
    redacted[-1] = "<prompt>"
    return redacted


def _resolve_schema_file() -> tuple[Path, Path | None]:
    schema_file = os.environ.get("AETHYME_EVAL_OUTPUT_SCHEMA_FILE")
    if schema_file:
        return Path(schema_file), None
    schema_json = os.environ.get("AETHYME_EVAL_OUTPUT_SCHEMA")
    if not schema_json:
        raise SystemExit("Missing AETHYME_EVAL_OUTPUT_SCHEMA_FILE/AETHYME_EVAL_OUTPUT_SCHEMA")
    temp_root = Path(tempfile.mkdtemp(prefix="aethyme-codex-schema-"))
    schema_path = temp_root / "schema.json"
    schema_path.write_text(schema_json, encoding="utf-8")
    return schema_path, temp_root


def _read_last_message(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    if not path.exists():
        return None, None
    contents = path.read_text(encoding="utf-8").strip()
    if not contents:
        return None, None
    try:
        payload = json.loads(contents)
    except json.JSONDecodeError:
        return None, contents
    return payload if isinstance(payload, dict) else None, contents


def _parse_usage(events_file: Path) -> dict[str, int | None]:
    usage: dict[str, int | None] = {
        "input_tokens": None,
        "output_tokens": None,
        "retries": None,
    }
    if not events_file.exists():
        return usage
    turn_failures = 0
    for line in events_file.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "turn.completed":
            usage_payload = event.get("usage", {})
            if isinstance(usage_payload, dict):
                input_tokens = usage_payload.get("input_tokens")
                output_tokens = usage_payload.get("output_tokens")
                usage["input_tokens"] = input_tokens if isinstance(input_tokens, int) else None
                usage["output_tokens"] = output_tokens if isinstance(output_tokens, int) else None
        if event.get("type") == "turn.failed":
            turn_failures += 1
    usage["retries"] = turn_failures if turn_failures else 0
    return usage


def _command_output_chars(events_file: Path) -> int:
    total = 0
    if not events_file.exists():
        return total
    for line in events_file.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        total += _sum_command_outputs(event)
    return total


def _detect_artifact_leakage(
    *,
    structured_output: dict[str, Any] | None,
    final_output_message: str | None,
    events_file: Path,
) -> dict[str, Any]:
    leaks: list[dict[str, str]] = []
    if structured_output is not None:
        leaks.extend(_collect_path_leaks(structured_output, "structured_output"))
    if final_output_message:
        leaks.extend(_collect_path_leaks(final_output_message, "final_output_message"))
    leaks.extend(_collect_command_output_path_leaks(events_file))

    return {
        "aethyme_path_leaked": bool(leaks),
        "markers": list(PATH_LEAK_MARKERS),
        "checked_surfaces": [
            "structured_output",
            "final_output_message",
            "command_output",
        ],
        "leak_count": len(leaks),
        "leaks": leaks[:MAX_REPORTED_LEAKS],
    }


def _regression_metrics(
    *,
    arm: str,
    structured_output: dict[str, Any] | None,
    usage: dict[str, int | None],
    command_output_chars: int,
    event_log_chars: int,
    artifact_leakage: dict[str, Any],
    events_file: Path,
) -> dict[str, Any]:
    token_estimate = _token_estimate(usage, event_log_chars)
    return {
        "token_estimate": token_estimate,
        "selected_file_count": _selected_file_count(structured_output),
        "snippet_count": _snippet_count(structured_output),
        "command_output_chars": command_output_chars,
        "aethyme_path_leaked": bool(artifact_leakage.get("aethyme_path_leaked")),
        "aethyme_invoked": _aethyme_invoked(events_file),
        "arm": arm,
    }


def _token_estimate(usage: dict[str, int | None], event_log_chars: int) -> int:
    input_tokens = usage.get("input_tokens")
    output_tokens = usage.get("output_tokens")
    if isinstance(input_tokens, int) and isinstance(output_tokens, int):
        return input_tokens + output_tokens
    return (event_log_chars + 3) // 4


def _selected_file_count(structured_output: dict[str, Any] | None) -> int:
    return _count_list_field(structured_output, "selected_files")


def _snippet_count(structured_output: dict[str, Any] | None) -> int:
    return _count_list_field(structured_output, "snippets")


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


def _aethyme_invoked(events_file: Path) -> bool:
    if not events_file.exists():
        return False
    for line in events_file.read_text(encoding="utf-8").splitlines():
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


def _collect_command_output_path_leaks(events_file: Path) -> list[dict[str, str]]:
    leaks: list[dict[str, str]] = []
    if not events_file.exists():
        return leaks
    for line_number, line in enumerate(
        events_file.read_text(encoding="utf-8").splitlines(), start=1
    ):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        leaks.extend(_collect_command_output_path_leaks_from_value(event, f"event[{line_number}]"))
    return leaks


def _collect_command_output_path_leaks_from_value(
    value: Any,
    path: str,
    *,
    key: str | None = None,
) -> list[dict[str, str]]:
    if isinstance(value, str):
        if key in COMMAND_OUTPUT_KEYS:
            return _collect_path_leaks(value, "command_output", path=path)
        return []
    if isinstance(value, list):
        leaks: list[dict[str, str]] = []
        for index, item in enumerate(value):
            leaks.extend(_collect_command_output_path_leaks_from_value(item, f"{path}[{index}]"))
        return leaks
    if isinstance(value, dict):
        leaks: list[dict[str, str]] = []
        for item_key, item in value.items():
            item_key_text = str(item_key)
            leaks.extend(
                _collect_command_output_path_leaks_from_value(
                    item,
                    f"{path}.{item_key_text}",
                    key=item_key_text,
                )
            )
        return leaks
    return []


def _collect_path_leaks(value: Any, source: str, *, path: str = "$") -> list[dict[str, str]]:
    if isinstance(value, str):
        marker = _matched_path_leak_marker(value)
        if marker is None:
            return []
        return [
            {
                "source": source,
                "path": path,
                "marker": marker,
                "excerpt": _leak_excerpt(value, marker),
            }
        ]
    if isinstance(value, list):
        leaks: list[dict[str, str]] = []
        for index, item in enumerate(value):
            leaks.extend(_collect_path_leaks(item, source, path=f"{path}[{index}]"))
        return leaks
    if isinstance(value, dict):
        leaks: list[dict[str, str]] = []
        for item_key, item in value.items():
            leaks.extend(_collect_path_leaks(item, source, path=f"{path}.{item_key}"))
        return leaks
    return []


def _matched_path_leak_marker(value: str) -> str | None:
    return next(
        (
            marker
            for marker in PATH_LEAK_MARKERS
            if _PATH_LEAK_PATTERNS[marker].search(value)
        ),
        None,
    )


def _leak_excerpt(value: str, marker: str) -> str:
    index = value.find(marker)
    if index < 0:
        return value[:120]
    start = max(0, index - 40)
    end = min(len(value), index + len(marker) + 80)
    prefix = "..." if start > 0 else ""
    suffix = "..." if end < len(value) else ""
    return f"{prefix}{value[start:end]}{suffix}"


def _sum_command_outputs(value: Any, *, key: str | None = None) -> int:
    if isinstance(value, str):
        return len(value) if key in COMMAND_OUTPUT_KEYS else 0
    if isinstance(value, list):
        return sum(_sum_command_outputs(item) for item in value)
    if isinstance(value, dict):
        return sum(
            _sum_command_outputs(item, key=str(item_key)) for item_key, item in value.items()
        )
    return 0


def _last_error(events_file: Path) -> str | None:
    if not events_file.exists():
        return None
    last_error: str | None = None
    for line in events_file.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "error" and isinstance(event.get("message"), str):
            last_error = event["message"]
        if event.get("type") == "turn.failed":
            error = event.get("error")
            if isinstance(error, dict) and isinstance(error.get("message"), str):
                last_error = error["message"]
    return last_error


if __name__ == "__main__":
    raise SystemExit(main())
