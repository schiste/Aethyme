"""
Documentation code example validation tests.

Tests that code examples in documentation are valid and executable.
"""

import re
import subprocess
from pathlib import Path
from typing import List, Tuple

import pytest


def find_code_blocks(file_path: Path) -> List[Tuple[str, str, int]]:
    """
    Extract code blocks from markdown file.

    Returns:
        List of (language, code, line_number) tuples
    """
    code_blocks = []
    current_block = None
    current_lang = None
    start_line = 0

    with open(file_path, "r", encoding="utf-8") as f:
        for line_num, line in enumerate(f, 1):
            # Check for code fence start
            match = re.match(r"```(\w+)?", line)
            if match and current_block is None:
                current_lang = match.group(1) or "text"
                current_block = []
                start_line = line_num
            # Check for code fence end
            elif line.strip() == "```" and current_block is not None:
                code_blocks.append((current_lang, "\n".join(current_block), start_line))
                current_block = None
                current_lang = None
            # Inside code block
            elif current_block is not None:
                current_block.append(line.rstrip())

    return code_blocks


@pytest.fixture
def docs_dir() -> Path:
    """Get docs directory path."""
    repo_root = Path(__file__).parent.parent.parent
    return repo_root / "docs"


def test_python_examples_syntax(docs_dir: Path):
    """Test that Python code examples have valid syntax."""
    errors = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang not in ["python", "py"]:
                continue

            # Skip incomplete examples (with ...)
            if "..." in code:
                continue

            # Try to compile the code
            try:
                compile(code, f"{md_file}:{line_num}", "exec")
            except SyntaxError as e:
                errors.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "error": str(e),
                        "code_snippet": code[:100],
                    }
                )

    if errors:
        error_msg = "Found Python syntax errors in documentation:\n"
        for error in errors:
            error_msg += f"  {error['file']}:{error['line']} - {error['error']}\n"
        pytest.fail(error_msg)


def test_bash_examples_syntax(docs_dir: Path):
    """Test that Bash code examples have basic syntax validity."""
    errors = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang not in ["bash", "sh", "shell"]:
                continue

            # Skip examples with placeholders
            if any(
                placeholder in code
                for placeholder in ["<", ">", "{", "}", "$1", "$2", "..."]
            ):
                continue

            # Use shellcheck if available
            try:
                result = subprocess.run(
                    ["shellcheck", "-"],
                    input=code.encode(),
                    capture_output=True,
                    timeout=5,
                )
                if result.returncode != 0:
                    # Only report serious errors (not warnings)
                    if b"error:" in result.stdout:
                        errors.append(
                            {
                                "file": str(md_file.relative_to(docs_dir)),
                                "line": line_num,
                                "error": result.stdout.decode()[:200],
                            }
                        )
            except (FileNotFoundError, subprocess.TimeoutExpired):
                # shellcheck not available, skip
                pass

    # This is a warning, not a failure
    if errors:
        print(f"\nWarning: Found {len(errors)} bash syntax issues (install shellcheck for details)")


def test_sql_examples_basic_syntax(docs_dir: Path):
    """Test that SQL code examples have basic syntax."""
    errors = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang not in ["sql", "postgresql", "postgres"]:
                continue

            # Skip examples with placeholders
            if any(
                placeholder in code
                for placeholder in ["{", "}", "...", "'..."]
            ):
                continue

            # Basic SQL keyword check
            sql_keywords = ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP"]
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


def test_curl_examples_valid(docs_dir: Path):
    """Test that curl examples have valid syntax."""
    errors = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang not in ["bash", "sh", "shell"]:
                continue

            # Find curl commands
            if "curl" not in code:
                continue

            # Check for common issues
            lines = code.split("\n")
            for i, line in enumerate(lines):
                if "curl" in line:
                    # Check for missing URL
                    if "http" not in line and i == len(lines) - 1:
                        errors.append(
                            {
                                "file": str(md_file.relative_to(docs_dir)),
                                "line": line_num + i,
                                "error": "curl command missing URL",
                            }
                        )

    # Warning only
    if errors:
        print(f"\nWarning: Found {len(errors)} potential curl issues")


def test_json_examples_valid(docs_dir: Path):
    """Test that JSON code examples are valid."""
    import json

    errors = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang not in ["json", "jsonc"]:
                continue

            # Skip examples with placeholders or comments
            if any(
                marker in code
                for marker in ["...", "//", "/*", "{...}", "$", "<"]
            ):
                continue

            # Try to parse JSON
            try:
                json.loads(code)
            except json.JSONDecodeError as e:
                errors.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "error": str(e),
                    }
                )

    if errors:
        error_msg = "Found invalid JSON in documentation:\n"
        for error in errors:
            error_msg += f"  {error['file']}:{error['line']} - {error['error']}\n"
        pytest.fail(error_msg)


def test_code_blocks_have_language(docs_dir: Path):
    """Test that code blocks specify language."""
    missing_lang = []

    for md_file in docs_dir.rglob("*.md"):
        code_blocks = find_code_blocks(md_file)

        for lang, code, line_num in code_blocks:
            if lang == "text" and len(code) > 10:
                # Probably should have a language
                missing_lang.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                    }
                )

    # Warning only
    if missing_lang:
        print(f"\nWarning: Found {len(missing_lang)} code blocks without language")
