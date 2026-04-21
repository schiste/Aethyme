#!/usr/bin/env python3
"""Run a single Aethyme eval prompt through Codex CLI and emit runner JSON."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


def main() -> int:
    prompt = os.environ.get("AETHYME_EVAL_PROMPT", "")
    repo_path = Path(os.environ["AETHYME_EVAL_REPO"]).expanduser().resolve()
    schema_file, schema_cleanup_dir = _resolve_schema_file()
    tool_repo = Path(os.environ.get("AETHYME_EVAL_TOOL_REPO", Path(__file__).resolve().parents[2])).resolve()

    try:
        with tempfile.TemporaryDirectory(prefix="aethyme-codex-eval-") as temp_dir:
            temp_root = Path(temp_dir)
            events_file = temp_root / "events.jsonl"
            last_message_file = temp_root / "last-message.json"

            command = [
                "codex",
                "exec",
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
                str(tool_repo),
                "--add-dir",
                "/tmp",
                "-C",
                str(repo_path),
                prompt,
            ]
            result = subprocess.run(
                command,
                check=False,
                capture_output=True,
                text=True,
                env=os.environ.copy(),
            )
            events_file.write_text(result.stdout, encoding="utf-8")
            structured_output, final_output_message = _read_last_message(last_message_file)
            usage = _parse_usage(events_file)
            error_message = _last_error(events_file)
            payload = {
                "input_tokens": usage.get("input_tokens"),
                "output_tokens": usage.get("output_tokens"),
                "retries": usage.get("retries"),
                "review_burden": None,
                "final_output_message": final_output_message or error_message,
                "structured_output": structured_output,
            }
            print(json.dumps(payload))
            if result.stderr:
                sys.stderr.write(result.stderr)
            if result.returncode != 0 and error_message:
                sys.stderr.write(f"\nCodex error: {error_message}\n")
            return result.returncode
    finally:
        if schema_cleanup_dir is not None:
            shutil.rmtree(schema_cleanup_dir, ignore_errors=True)


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
