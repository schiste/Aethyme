"""
Documentation link validation tests.

Tests that all internal links in documentation are valid.
"""

import os
import re
from pathlib import Path
from typing import List, Tuple

import pytest


def find_markdown_files(docs_dir: Path) -> List[Path]:
    """Find all markdown files in docs directory."""
    return list(docs_dir.rglob("*.md"))


def extract_links(file_path: Path) -> List[Tuple[str, int]]:
    """
    Extract markdown links from file.

    Returns:
        List of (link, line_number) tuples
    """
    links = []
    link_pattern = re.compile(r"\[([^\]]+)\]\(([^\)]+)\)")

    with open(file_path, "r", encoding="utf-8") as f:
        for line_num, line in enumerate(f, 1):
            for match in link_pattern.finditer(line):
                link = match.group(2)
                links.append((link, line_num))

    return links


def is_external_link(link: str) -> bool:
    """Check if link is external (http/https)."""
    return link.startswith("http://") or link.startswith("https://")


def is_anchor_link(link: str) -> bool:
    """Check if link is an anchor (#section)."""
    return link.startswith("#")


@pytest.fixture
def docs_dir() -> Path:
    """Get docs directory path."""
    repo_root = Path(__file__).parent.parent.parent
    return repo_root / "docs"


def test_docs_directory_exists(docs_dir: Path):
    """Test that docs directory exists."""
    assert docs_dir.exists(), f"Docs directory not found: {docs_dir}"
    assert docs_dir.is_dir(), f"Docs path is not a directory: {docs_dir}"


def test_markdown_files_exist(docs_dir: Path):
    """Test that markdown files exist in docs."""
    md_files = find_markdown_files(docs_dir)
    assert len(md_files) > 0, "No markdown files found in docs/"


def test_internal_links_valid(docs_dir: Path):
    """Test that all internal links point to existing files."""
    broken_links = []

    for md_file in find_markdown_files(docs_dir):
        links = extract_links(md_file)

        for link, line_num in links:
            # Skip external and anchor links
            if is_external_link(link) or is_anchor_link(link):
                continue

            # Resolve relative path
            target_path = (md_file.parent / link).resolve()

            # Check if target exists
            if not target_path.exists():
                broken_links.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "link": link,
                        "target": str(target_path),
                    }
                )

    # Report broken links
    if broken_links:
        error_msg = "Found broken internal links:\n"
        for broken in broken_links:
            error_msg += f"  {broken['file']}:{broken['line']} -> {broken['link']}\n"
        pytest.fail(error_msg)


def test_no_absolute_github_links(docs_dir: Path):
    """Test that docs use relative links, not absolute GitHub links."""
    absolute_github_links = []

    for md_file in find_markdown_files(docs_dir):
        links = extract_links(md_file)

        for link, line_num in links:
            # Check for GitHub URLs pointing to this repo
            if "github.com/aeptus/repograph" in link and "/blob/" in link:
                absolute_github_links.append(
                    {
                        "file": str(md_file.relative_to(docs_dir)),
                        "line": line_num,
                        "link": link,
                    }
                )

    # Report absolute links
    if absolute_github_links:
        error_msg = "Found absolute GitHub links (use relative links instead):\n"
        for link_info in absolute_github_links:
            error_msg += f"  {link_info['file']}:{link_info['line']} -> {link_info['link']}\n"
        pytest.fail(error_msg)


def test_required_documentation_exists(docs_dir: Path):
    """Test that required documentation files exist."""
    required_docs = [
        "getting-started/quickstart.md",
        "getting-started/onboarding.md",
        "reference/api.md",
        "reference/cli.md",
        "guides/troubleshooting.md",
        "runbooks/index-failure.md",
        "runbooks/rollback.md",
        "security/security-overview.md",
    ]

    missing_docs = []
    for doc_path in required_docs:
        full_path = docs_dir / doc_path
        if not full_path.exists():
            missing_docs.append(doc_path)

    if missing_docs:
        pytest.fail(f"Missing required documentation:\n  " + "\n  ".join(missing_docs))


def test_runbooks_have_standard_sections(docs_dir: Path):
    """Test that runbooks have standard sections."""
    runbooks_dir = docs_dir / "runbooks"
    if not runbooks_dir.exists():
        pytest.skip("Runbooks directory does not exist")

    required_sections = [
        "## Overview",
        "## Symptoms",
        # Either "## Diagnostic" or "## Detection"
        ("## Diagnostic", "## Detection"),
    ]

    for runbook in runbooks_dir.glob("*.md"):
        with open(runbook, "r", encoding="utf-8") as f:
            content = f.read()

        for section in required_sections:
            if isinstance(section, tuple):
                # Check if any of the alternatives exists
                if not any(s in content for s in section):
                    pytest.fail(
                        f"Runbook {runbook.name} missing one of: {', '.join(section)}"
                    )
            else:
                if section not in content:
                    pytest.fail(f"Runbook {runbook.name} missing section: {section}")


def test_documentation_has_last_updated(docs_dir: Path):
    """Test that documentation files have last updated date."""
    missing_dates = []

    for md_file in find_markdown_files(docs_dir):
        # Skip certain files
        if md_file.name in ["README.md", "INDEX.md"]:
            continue

        with open(md_file, "r", encoding="utf-8") as f:
            content = f.read()

        # Look for "Last Updated" or "Last Reviewed"
        if "Last Updated:" not in content and "Last Reviewed:" not in content:
            missing_dates.append(str(md_file.relative_to(docs_dir)))

    if missing_dates:
        # Warning only, not failure
        print(f"\nWarning: {len(missing_dates)} docs missing 'Last Updated' date:")
        for doc in missing_dates[:10]:  # Show first 10
            print(f"  {doc}")
