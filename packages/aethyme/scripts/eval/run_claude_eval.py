#!/usr/bin/env python3
"""Run a single Aethyme eval prompt through Claude Code CLI and emit runner JSON."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


MODEL = "sonnet"


def main() -> int:
    prompt = os.environ.get("AETHYME_EVAL_PROMPT", "")
    if not prompt:
        prompt_file = os.environ.get("AETHYME_EVAL_PROMPT_FILE")
        if prompt_file:
            prompt = Path(prompt_file).read_text(encoding="utf-8")

    repo_path = Path(os.environ["AETHYME_EVAL_REPO"]).expanduser().resolve()
    schema_file = _resolve_schema_file()
    nav_context = _load_navigation_context()

    system_parts = ["You are an expert code analyst. Answer precisely in valid JSON only."]
    if nav_context:
        system_parts.append(
            f"Navigation context (graph-mediated):\n{nav_context}"
        )

    command = [
        "claude",
        "--print",
        "--output-format", "json",
        "--model", MODEL,
        "--no-session-persistence",
        "--dangerously-skip-permissions",
        "--add-dir", str(repo_path),
        "--system-prompt", "\n\n".join(system_parts),
    ]
    if schema_file:
        schema_text = Path(schema_file).read_text(encoding="utf-8")
        command.extend(["--json-schema", schema_text])

    command.append(prompt)

    env = os.environ.copy()
    env.pop("CLAUDECODE", None)  # allow nested invocation

    result = subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
        cwd=str(repo_path),
        env=env,
    )

    if result.returncode != 0:
        payload = {
            "input_tokens": None,
            "output_tokens": None,
            "retries": 0,
            "review_burden": None,
            "final_output_message": result.stderr or result.stdout or "Claude CLI error",
            "structured_output": None,
        }
        print(json.dumps(payload))
        sys.stderr.write(result.stderr or "")
        return result.returncode

    events = _parse_events(result.stdout)
    result_event = _find_event(events, "result")
    structured_output, final_message = _extract_response(events)
    usage = _extract_usage(events)
    tool_calls = _extract_tool_calls(events)

    payload = {
        "input_tokens": usage.get("input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "retries": 0,
        "review_burden": None,
        "final_output_message": final_message,
        "structured_output": structured_output,
        "tool_calls": tool_calls or None,
    }
    print(json.dumps(payload))
    if result.stderr:
        sys.stderr.write(result.stderr)
    return 0


def _resolve_schema_file() -> str | None:
    schema_file = os.environ.get("AETHYME_EVAL_OUTPUT_SCHEMA_FILE")
    if schema_file and Path(schema_file).exists():
        return schema_file
    schema_json = os.environ.get("AETHYME_EVAL_OUTPUT_SCHEMA")
    if not schema_json:
        return None
    tmp = tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", prefix="aethyme-schema-", delete=False
    )
    tmp.write(schema_json)
    tmp.close()
    return tmp.name


def _load_navigation_context() -> str | None:
    nav_file = os.environ.get("AETHYME_EVAL_NAVIGATION_CONTEXT_FILE")
    if nav_file and Path(nav_file).exists():
        return Path(nav_file).read_text(encoding="utf-8")
    return os.environ.get("AETHYME_EVAL_NAVIGATION_CONTEXT")


def _parse_events(stdout: str) -> list[dict[str, Any]]:
    """Parse the JSON events array from claude --print --output-format json."""
    try:
        parsed = json.loads(stdout)
    except json.JSONDecodeError:
        return []
    if isinstance(parsed, list):
        return [e for e in parsed if isinstance(e, dict)]
    if isinstance(parsed, dict):
        return [parsed]
    return []


def _find_event(events: list[dict[str, Any]], event_type: str) -> dict[str, Any] | None:
    for event in reversed(events):
        if event.get("type") == event_type:
            return event
    return None


def _extract_response(events: list[dict[str, Any]]) -> tuple[dict[str, Any] | None, str | None]:
    """Extract structured output and text from the events array."""
    result_event = _find_event(events, "result")
    if result_event:
        result_text = result_event.get("result", "")
        parsed = _try_parse_json(result_text)
        if parsed is not None:
            return parsed, result_text
        return None, result_text or None

    # Fall back to assistant message
    assistant_event = _find_event(events, "assistant")
    if assistant_event:
        message = assistant_event.get("message", {})
        content = message.get("content", [])
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = block.get("text", "")
                parsed = _try_parse_json(text)
                return parsed, text

    return None, None


def _extract_usage(events: list[dict[str, Any]]) -> dict[str, int | None]:
    """Extract token usage from the result event."""
    usage: dict[str, int | None] = {"input_tokens": None, "output_tokens": None}
    result_event = _find_event(events, "result")
    if not result_event:
        return usage

    usage_data = result_event.get("usage", {})
    if not isinstance(usage_data, dict):
        return usage

    # Total input = input_tokens + cache_creation + cache_read
    input_tokens = _as_int(usage_data.get("input_tokens"))
    cache_creation = _as_int(usage_data.get("cache_creation_input_tokens"))
    cache_read = _as_int(usage_data.get("cache_read_input_tokens"))
    if input_tokens is not None:
        total_input = input_tokens
        if cache_creation:
            total_input += cache_creation
        if cache_read:
            total_input += cache_read
        usage["input_tokens"] = total_input

    usage["output_tokens"] = _as_int(usage_data.get("output_tokens"))
    return usage


def _as_int(value: object) -> int | None:
    return value if isinstance(value, int) else None


def _try_parse_json(text: str) -> dict[str, Any] | None:
    text = text.strip()
    if text.startswith("```"):
        lines = text.splitlines()
        lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        text = "\n".join(lines).strip()
    try:
        obj = json.loads(text)
        return obj if isinstance(obj, dict) else None
    except json.JSONDecodeError:
        return None


def _extract_tool_calls(events: list[dict[str, Any]]) -> list[dict[str, str]]:
    """Walk Claude JSON events and extract tool_use blocks from assistant messages."""
    tool_calls: list[dict[str, str]] = []
    for event in events:
        if event.get("type") != "assistant":
            continue
        message = event.get("message", {})
        content = message.get("content", [])
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            name = block.get("name", "unknown")
            input_data = block.get("input", {})
            tool_calls.append({
                "tool": name,
                "input_summary": _summarize_tool_input(name, input_data),
            })
    return tool_calls


def _summarize_tool_input(name: str, input_data: Any) -> str:
    """Return a short human-readable summary of the tool input."""
    if not isinstance(input_data, dict):
        return str(input_data)[:80] if input_data else ""

    # Well-known tools with clear primary fields
    if name in ("Read", "Write", "Edit"):
        return str(input_data.get("file_path", ""))
    if name == "Glob":
        return str(input_data.get("pattern", ""))
    if name == "Grep":
        return str(input_data.get("pattern", ""))
    if name == "Bash":
        cmd = str(input_data.get("command", ""))
        return cmd[:80] + ("..." if len(cmd) > 80 else "")

    # Fallback: first string value, truncated
    for value in input_data.values():
        if isinstance(value, str) and value.strip():
            return value[:80] + ("..." if len(value) > 80 else "")
    return ""


if __name__ == "__main__":
    raise SystemExit(main())
