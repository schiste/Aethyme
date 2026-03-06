"""Documentation code example validation tests."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import TypedDict

import pytest


class CodeSyntaxError(TypedDict):
    file: str
    line: int
    error: str
    code_snippet: str


class BashSyntaxIssue(TypedDict):
    file: str
    line: int
    error: str


class SQLSuspiciousBlock(TypedDict):
    file: str
    line: int
    error: str


class CurlIssue(TypedDict):
    file: str
    line: int
    error: str


class JSONSyntaxError(TypedDict):
    file: str
    line: int
    error: str


class MissingLanguageBlock(TypedDict):
    file: str
    line: int


def find_code_blocks(file_path: Path) -> list[tuple[str, str, int]]:
    """Extract markdown code fences as (language, code, start_line)."""
    code_blocks: list[tuple[str, str, int]] = []
    current_block: list[str] | None = None
    current_lang: str | None = None
    start_line = 0

    with file_path.open(encoding="utf-8") as file:
        for line_num, line in enumerate(file, 1):
            match = re.match(r"```(\w+)?", line)
            if match and current_block is None:
                current_lang = match.group(1) or "text"
                current_block = []
                start_line = line_num
            elif line.strip() == "```" and current_block is not None and current_lang is not None:
                code_blocks.append((current_lang, "\n".join(current_block), start_line))
                current_block = None
                current_lang = None
            elif current_block is not None:
                current_block.append(line.rstrip())

    return code_blocks


@pytest.fixture
def docs_dir() -> Path:
    """Get docs directory path."""
    repo_root = Path(__file__).parent.parent.parent
    return repo_root / "docs"


def test_python_examples_syntax(docs_dir: Path) -> None:
    """Python code examples must compile."""
    errors: list[CodeSyntaxError] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang not in {"python", "py"}:
                continue
            if "..." in code:
                continue

            try:
                compile(code, f"{md_file}:{line_num}", "exec")
            except SyntaxError as error:
                errors.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "error": str(error),
                        "code_snippet": code[:100],
                    }
                )

    if errors:
        error_lines = ["Found Python syntax errors in documentation:"]
        for error in errors:
            error_lines.append(f"  {error['file']}:{error['line']} - {error['error']}")
        pytest.fail("\n".join(error_lines))


def test_bash_examples_syntax(docs_dir: Path) -> None:
    """Bash code examples should pass shellcheck when available."""
    errors: list[BashSyntaxIssue] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang not in {"bash", "sh", "shell"}:
                continue
            if any(placeholder in code for placeholder in ["<", ">", "{", "}", "$1", "$2", "..."]):
                continue

            try:
                result = subprocess.run(
                    ["shellcheck", "-"],
                    input=code.encode(),
                    capture_output=True,
                    timeout=5,
                )
                if result.returncode != 0 and b"error:" in result.stdout:
                    errors.append(
                        {
                            "file": str(md_file.relative_to(docs_dir)),
                            "line": line_num,
                            "error": result.stdout.decode(errors="replace")[:200],
                        }
                    )
            except (FileNotFoundError, subprocess.TimeoutExpired):
                pass

    if errors:
        print(f"\nWarning: Found {len(errors)} bash syntax issues (install shellcheck for details)")


def test_sql_examples_basic_syntax(docs_dir: Path) -> None:
    """SQL code examples should contain at least one SQL keyword."""
    errors: list[SQLSuspiciousBlock] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang not in {"sql", "postgresql", "postgres"}:
                continue
            if any(placeholder in code for placeholder in ["{", "}", "...", "'...'"]):
                continue

            sql_keywords = {"SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP"}
            has_keyword = any(keyword in code.upper() for keyword in sql_keywords)
            if not has_keyword:
                errors.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "error": "SQL block missing SQL keywords",
                    }
                )

    if errors:
        print(f"\nWarning: Found {len(errors)} suspicious SQL blocks")


def test_curl_examples_valid(docs_dir: Path) -> None:
    """curl examples should include a URL on command lines."""
    errors: list[CurlIssue] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang not in {"bash", "sh", "shell"}:
                continue
            if "curl" not in code:
                continue

            lines = code.split("\n")
            for index, line in enumerate(lines):
                if "curl" in line and "http" not in line and index == len(lines) - 1:
                    errors.append(
                        {
                            "file": str(md_file.relative_to(docs_dir)),
                            "line": line_num + index,
                            "error": "curl command missing URL",
                        }
                    )

    if errors:
        print(f"\nWarning: Found {len(errors)} potential curl issues")


def test_json_examples_valid(docs_dir: Path) -> None:
    """JSON code examples must parse when not placeholder snippets."""
    errors: list[JSONSyntaxError] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang not in {"json", "jsonc"}:
                continue
            if any(marker in code for marker in ["...", "//", "/*", "{...}", "$", "<"]):
                continue

            try:
                json.loads(code)
            except json.JSONDecodeError as error:
                errors.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "error": str(error),
                    }
                )

    if errors:
        error_lines = ["Found invalid JSON in documentation:"]
        for error in errors:
            error_lines.append(f"  {error['file']}:{error['line']} - {error['error']}")
        pytest.fail("\n".join(error_lines))


def test_code_blocks_have_language(docs_dir: Path) -> None:
    """Code blocks should declare a language when non-trivial."""
    missing_lang: list[MissingLanguageBlock] = []

    for md_file in docs_dir.rglob("*.md"):
        for lang, code, line_num in find_code_blocks(md_file):
            if lang == "text" and len(code) > 10:
                missing_lang.append({"file": str(md_file.relative_to(docs_dir)), "line": line_num})

    if missing_lang:
        print(f"\nWarning: Found {len(missing_lang)} code blocks without language")
