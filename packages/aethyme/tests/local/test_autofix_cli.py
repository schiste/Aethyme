"""Implementation-blind CLI tests for `aethyme autofix` (Phase 5).

Replaces tests/autofixers/ after the native flip: the same assertions
run against the router binary as a subprocess, so they hold for
whichever implementation answers (aethyme-quality since the Phase 5
flip). Safety-engine, patch, fixer, and PR-helper behavior is covered by
the Rust unit tests in rust/crates/aethyme-quality and by the parity
corpus that gated the flip.

Nothing here touches a real remote: the PR-mode tests either run outside
a git repository or stop at the approval gate.
"""

from __future__ import annotations

from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme


def build_fixable_repo(tmp_path: Path) -> Path:
    """A repo with one fixable item per fixer plus protected paths."""
    repo = tmp_path / "autofix-repo"
    (repo / "src").mkdir(parents=True)
    (repo / "src" / "module.py").write_text('"""Module purpose."""\n\nx = 1\n')
    (repo / "docs").mkdir()
    (repo / "docs" / "page.md").write_text("[Absolute](/src/module.py)\n")
    (repo / "ui").mkdir()
    (repo / "ui" / "Panel.tsx").write_text(
        "import React from 'react';\n"
        "export function Panel() { return <button>Save Changes</button>; }\n"
    )
    # Protected paths the safety engine must refuse to patch.
    (repo / "package.json").write_text("{}\n")
    (repo / "Cargo.lock").write_text("# lock\n")
    (repo / "node_modules" / "pkg").mkdir(parents=True)
    (repo / "node_modules" / "pkg" / "index.js").write_text("module.exports = 1;\n")
    return repo


def build_clean_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "clean-repo"
    repo.mkdir()
    (repo / "README.md").write_text("# Nothing to fix here\n")
    return repo


class TestAutofixCommand:
    def test_dry_run_is_the_default_mode(self, tmp_path: Path) -> None:
        result = invoke_aethyme(["autofix", str(build_fixable_repo(tmp_path))])
        assert result.exit_code == 0
        assert "Mode: DRY RUN" in result.output
        assert "Changes Preview (Dry Run)" in result.output
        assert "Run with --apply to apply these changes" in result.output

    def test_dry_run_writes_nothing(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        before = {p: p.read_bytes() for p in repo.rglob("*") if p.is_file()}
        result = invoke_aethyme(["autofix", str(repo), "--dry-run"])
        assert result.exit_code == 0
        after = {p: p.read_bytes() for p in repo.rglob("*") if p.is_file()}
        assert before == after

    def test_summary_reports_risk_buckets(self, tmp_path: Path) -> None:
        result = invoke_aethyme(["autofix", str(build_fixable_repo(tmp_path))])
        for line in ["Total files:", "Low risk:", "Medium risk:", "High risk:"]:
            assert line in result.output

    def test_clean_repo_reports_no_fixes(self, tmp_path: Path) -> None:
        result = invoke_aethyme(["autofix", str(build_clean_repo(tmp_path))])
        assert result.exit_code == 0
        assert "No fixes needed!" in result.output

    def test_fix_type_selects_a_single_scanner(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        result = invoke_aethyme(["autofix", str(repo), "--fix-type", "docs"])
        assert result.exit_code == 0
        assert "Scanning for documentation issues..." in result.output
        assert "Scanning for link issues..." not in result.output

    def test_every_fix_type_choice_runs(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        for choice in ["all", "docs", "links", "selectors", "i18n", "format"]:
            result = invoke_aethyme(["autofix", str(repo), "--fix-type", choice])
            assert result.exit_code == 0, choice

    def test_apply_requires_approval_and_writes_nothing(self, tmp_path: Path) -> None:
        """The gate: newly created docs are medium risk, so --apply stops."""
        repo = build_fixable_repo(tmp_path)
        before = {p: p.read_bytes() for p in repo.rglob("*") if p.is_file()}
        result = invoke_aethyme(["autofix", str(repo), "--apply", "--fix-type", "docs"])
        # stdin is closed, so the confirmation prompt aborts.
        assert result.exit_code == 1
        assert "files require approval" in result.output
        assert "Aborted!" in result.output
        after = {p: p.read_bytes() for p in repo.rglob("*") if p.is_file()}
        assert before == after

    def test_skip_approval_applies_the_patches(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        result = invoke_aethyme(
            ["autofix", str(repo), "--apply", "--skip-approval", "--fix-type", "docs"]
        )
        assert result.exit_code == 0
        assert "Applied " in result.output
        assert (repo / "src" / "FOLDER.md").exists()

    def test_protected_paths_are_never_patched(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        result = invoke_aethyme(["autofix", str(repo), "--apply", "--skip-approval"])
        assert result.exit_code == 0
        # A lockfile, a manifest, and a build directory: no FOLDER.md
        # may appear next to them, and their contents are untouched.
        assert (repo / "package.json").read_text() == "{}\n"
        assert (repo / "Cargo.lock").read_text() == "# lock\n"
        assert not (repo / "node_modules" / "pkg" / "FOLDER.md").exists()

    def test_dry_run_wins_over_apply(self, tmp_path: Path) -> None:
        repo = build_fixable_repo(tmp_path)
        result = invoke_aethyme(["autofix", str(repo), "--dry-run", "--apply"])
        assert result.exit_code == 0
        assert "Mode: DRY RUN" in result.output
        assert not (repo / "src" / "FOLDER.md").exists()

    def test_pr_mode_refuses_a_dirty_working_tree(self, tmp_path: Path) -> None:
        # Not a git repository at all: `git status` fails, which the
        # command reports as an unclean tree.
        result = invoke_aethyme(["autofix", str(build_fixable_repo(tmp_path)), "--pr"])
        assert result.exit_code == 0
        assert "Working tree has uncommitted changes" in result.output

    def test_missing_argument_is_a_usage_error(self) -> None:
        result = invoke_aethyme(["autofix"])
        assert result.exit_code == 2
        assert "Missing argument 'REPO_PATH'." in result.output

    def test_invalid_repo_path(self) -> None:
        result = invoke_aethyme(["autofix", "/nonexistent/path"])
        assert result.exit_code == 2
        assert "does not exist" in result.output

    def test_invalid_fix_type(self, tmp_path: Path) -> None:
        result = invoke_aethyme(
            ["autofix", str(build_clean_repo(tmp_path)), "--fix-type", "bogus"]
        )
        assert result.exit_code == 2
        assert "is not one of 'all', 'docs', 'links', 'selectors', 'i18n', 'format'." in (
            result.output
        )

    def test_help_names_the_options(self) -> None:
        result = invoke_aethyme(["autofix", "--help"])
        assert result.exit_code == 0
        for flag in ["--dry-run", "--apply", "--pr", "--fix-type", "--skip-approval"]:
            assert flag in result.output
