"""Documentation link validation tests."""

from __future__ import annotations

import re
from pathlib import Path
from typing import TypedDict

import pytest


class BrokenLink(TypedDict):
    file: str
    line: int
    link: str
    target: str


class AbsoluteGitHubLink(TypedDict):
    file: str
    line: int
    link: str


def find_markdown_files(docs_dir: Path) -> list[Path]:
    """Find all markdown files in docs directory."""
    return list(docs_dir.rglob("*.md"))


def extract_links(file_path: Path) -> list[tuple[str, int]]:
    """Extract markdown links from a file."""
    links: list[tuple[str, int]] = []
    link_pattern = re.compile(r"\[([^\]]+)\]\(([^\)]+)\)")

    with file_path.open(encoding="utf-8") as file:
        for line_num, line in enumerate(file, 1):
            for match in link_pattern.finditer(line):
                links.append((match.group(2), line_num))

    return links


def is_external_link(link: str) -> bool:
    return link.startswith("http://") or link.startswith("https://")


def is_anchor_link(link: str) -> bool:
    return link.startswith("#")


@pytest.fixture
def docs_dir() -> Path:
    """Get docs directory path."""
    repo_root = Path(__file__).parent.parent.parent
    return repo_root / "docs"


def test_docs_directory_exists(docs_dir: Path) -> None:
    assert docs_dir.exists(), f"Docs directory not found: {docs_dir}"
    assert docs_dir.is_dir(), f"Docs path is not a directory: {docs_dir}"


def test_markdown_files_exist(docs_dir: Path) -> None:
    md_files = find_markdown_files(docs_dir)
    assert len(md_files) > 0, "No markdown files found in docs/"


def test_internal_links_valid(docs_dir: Path) -> None:
    """Internal links in human-curated docs must resolve.

    Auto-generated eval reports (`docs/reports/`) are excluded: they
    embed paths to transient runtime artifacts (e.g. `/private/tmp/
    aethyme-eval-demo-xxx/`) that don't exist outside the run, by
    design. Subjecting them to the same contract as hand-written
    documentation conflates human authoring discipline with runtime
    capture.
    """
    broken_links: list[BrokenLink] = []

    for md_file in find_markdown_files(docs_dir):
        if "reports" in md_file.relative_to(docs_dir).parts:
            continue
        for link, line_num in extract_links(md_file):
            if is_external_link(link) or is_anchor_link(link):
                continue

            target_path = (md_file.parent / link).resolve()
            if not target_path.exists():
                broken_links.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "link": link,
                        "target": str(target_path),
                    }
                )

    if broken_links:
        error_lines = ["Found broken internal links:"]
        for broken in broken_links:
            error_lines.append(f"  {broken['file']}:{broken['line']} -> {broken['link']}")
        pytest.fail("\n".join(error_lines))


def test_no_absolute_github_links(docs_dir: Path) -> None:
    absolute_github_links: list[AbsoluteGitHubLink] = []

    for md_file in find_markdown_files(docs_dir):
        for link, line_num in extract_links(md_file):
            if "github.com/aeptus/aethyme" in link and "/blob/" in link:
                absolute_github_links.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "link": link,
                    }
                )

    if absolute_github_links:
        error_lines = ["Found absolute GitHub links (use relative links instead):"]
        for link_info in absolute_github_links:
            error_lines.append(
                f"  {link_info['file']}:{link_info['line']} -> {link_info['link']}"
            )
        pytest.fail("\n".join(error_lines))


def test_required_documentation_exists(docs_dir: Path) -> None:
    required_docs = [
        "getting-started/quickstart.md",
        "getting-started/onboarding.md",
        "reference/cli.md",
        "guides/testing.md",
        "guides/troubleshooting.md",
        "runbooks/index-failure.md",
        "runbooks/rollback.md",
    ]

    missing_docs: list[str] = []
    for doc_path in required_docs:
        if not (docs_dir / doc_path).exists():
            missing_docs.append(doc_path)

    if missing_docs:
        pytest.fail("Missing required documentation:\n  " + "\n  ".join(missing_docs))


def test_runbooks_have_standard_sections(docs_dir: Path) -> None:
    runbooks_dir = docs_dir / "runbooks"
    if not runbooks_dir.exists():
        pytest.skip("Runbooks directory does not exist")

    required_sections: list[str | tuple[str, str]] = [
        "## Overview",
        "## Symptoms",
        ("## Diagnostic", "## Detection"),
    ]

    for runbook in runbooks_dir.glob("*.md"):
        content = runbook.read_text(encoding="utf-8")

        for section in required_sections:
            if isinstance(section, tuple):
                if not any(option in content for option in section):
                    pytest.fail(f"Runbook {runbook.name} missing one of: {', '.join(section)}")
            elif section not in content:
                pytest.fail(f"Runbook {runbook.name} missing section: {section}")


def test_documentation_has_last_updated(docs_dir: Path) -> None:
    """Hand-curated docs must carry "Last Updated:" or "Last Reviewed:".

    Auto-generated eval reports (`docs/reports/`) are excluded: they're
    timestamped by filename (e.g. `20260507-191139-...md`), and
    forcing every auto-generated report through a "must have a stamp"
    contract is a category mistake. The filename is the stamp.
    """
    missing_dates: list[str] = []

    for md_file in find_markdown_files(docs_dir):
        if md_file.name in {"README.md", "INDEX.md"}:
            continue
        if "reports" in md_file.relative_to(docs_dir).parts:
            continue

        content = md_file.read_text(encoding="utf-8")
        if "Last Updated:" not in content and "Last Reviewed:" not in content:
            missing_dates.append(str(md_file.relative_to(docs_dir)))

    if missing_dates:
        pytest.fail("Missing last-updated metadata:\n  " + "\n  ".join(sorted(missing_dates)))
