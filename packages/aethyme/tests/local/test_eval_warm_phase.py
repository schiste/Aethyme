"""Tests for the eval plan's warm phase.

The warm phase pre-starts the engine daemon for the target Aethyme repo
so the leverage condition doesn't eat the ~108s map_build cost on
MediaWiki (or ~6s on Mockup) on its first `aethyme explore` call.

These tests exercise the *plan generation*, not the actual daemon
spawn — running a real daemon in the test suite would be slow and
flaky. The tests confirm the plan emits the warm phase in the right
slot with the right contents; integration is covered by running an
eval end-to-end.
"""

from __future__ import annotations

import pytest

from src.eval.orchestrator import generate_run_plan


def _plan(eval_type: str, target: str) -> dict:
    return generate_run_plan(eval_type=eval_type, target=target, model="haiku")


def test_warm_phase_present_in_plan():
    """Every eval-plan must include a warm phase between build-inputs and launch."""
    plan = _plan("bug-fix", "grc")
    phase_names = [ph["name"] for ph in plan["phases"]]
    assert "warm" in phase_names

    # Position: between build-inputs and launch
    build_idx = phase_names.index("build-inputs")
    warm_idx = phase_names.index("warm")
    launch_idx = phase_names.index("launch")
    assert build_idx < warm_idx < launch_idx, (
        f"warm phase must sit between build-inputs and launch; "
        f"got phase order {phase_names}"
    )


def test_warm_phase_contents_for_grc():
    plan = _plan("bug-fix", "grc")
    warm = next(ph for ph in plan["phases"] if ph["name"] == "warm")

    assert warm["engine_bin"].endswith("aethyme-engine-cli")
    assert "Mockup - Aethyme" in warm["aethyme_repo"]
    assert warm["log_path"].endswith(".aethyme/engine-daemon.log")
    assert warm["wait_for"] == "listening on"
    assert isinstance(warm["max_wait_seconds"], int)
    assert warm["max_wait_seconds"] >= 60, (
        "max_wait_seconds must be ≥60 to cover Mockup's 6s typical cold start "
        "comfortably AND the slower-than-expected case"
    )


def test_warm_phase_contents_for_mediawiki():
    plan = _plan("bug-fix-1", "mediawiki")
    warm = next(ph for ph in plan["phases"] if ph["name"] == "warm")

    assert "Mediawiki - Aethyme" in warm["aethyme_repo"]
    assert warm["max_wait_seconds"] >= 240, (
        "max_wait_seconds must be ≥240 to cover MediaWiki's ~125s cold start"
    )


def test_warm_cli_cmd_short_circuits_when_daemon_running():
    """The cli_cmd should check status FIRST, only start if needed.

    Re-running an eval shouldn't double-spawn the daemon. The shell
    sequence uses `daemon status || (daemon start && wait)` to
    short-circuit when already running.
    """
    plan = _plan("bug-fix", "grc")
    warm = next(ph for ph in plan["phases"] if ph["name"] == "warm")
    cmd = warm["cli_cmd"]
    assert "daemon status" in cmd
    assert "daemon start" in cmd
    assert "||" in cmd, "cli_cmd should use || to short-circuit on already-running daemon"
    assert "listening on" in cmd, "cli_cmd should grep for listening marker"


@pytest.mark.parametrize("eval_type,target", [("bug-fix", "grc"), ("bug-fix-1", "mediawiki")])
def test_warm_cli_cmd_quotes_paths_with_spaces(eval_type: str, target: str):
    """Regression: warm cli_cmd must shell-quote paths to handle spaces.

    Both target playgrounds have a space in their tool-variant path
    name ("Mockup - Aethyme", "Mediawiki - Aethyme"). The cli_cmd is
    fed to a shell; without quotes, the path splits across the space
    and the ``--repo`` value gets truncated.

    The adapter-driven warm phase must use the same quote-aware command
    renderer as register / condition commands. This test pins that
    contract so a future ad hoc ``.replace()`` path can't silently
    re-introduce the bug observed during the dead-code mediawiki
    smoke-test.
    """
    plan = _plan(eval_type, target)
    warm = next(ph for ph in plan["phases"] if ph["name"] == "warm")
    cmd = warm["cli_cmd"]
    repo_path = warm.get("aethyme_repo") or warm.get("target_repo")
    assert repo_path is not None, f"warm phase missing repo path field for {target}"
    quoted_double = f'"{repo_path}"'
    quoted_single = f"'{repo_path}'"
    assert (quoted_double in cmd) or (quoted_single in cmd), (
        f"warm cli_cmd for target={target!r} does not shell-quote the "
        f"repo path {repo_path!r}. Path contains a space — unquoted "
        f"occurrence will break the shell command. Warm commands must "
        f"go through ManifestToolAdapter.render_command()."
    )


@pytest.mark.parametrize(
    "eval_type,target",
    [
        ("bug-fix", "grc"),
        ("bug-fix-1", "mediawiki"),
        ("explain-repo", "grc"),
        ("dead-code", "mediawiki"),
        ("impact-analysis", "mediawiki"),
        ("feature-localization", "mediawiki"),
        ("config-audit", "mediawiki"),
        ("migration", "mediawiki"),
        ("navigation-ctf", "grc"),
    ],
)
def test_warm_phase_present_for_all_eval_types(eval_type: str, target: str):
    """Every eval type benefits from a warm daemon. Sanity check that no
    eval type accidentally drops the phase.
    """
    plan = _plan(eval_type, target)
    phase_names = [ph["name"] for ph in plan["phases"]]
    assert "warm" in phase_names, (
        f"eval-type {eval_type} (target {target}) is missing the warm "
        f"phase; got phases {phase_names}"
    )


def test_phase_count_is_nine():
    """Regression: the post-2026-05-09 plan has 9 phases (was 8 pre-warm)."""
    plan = _plan("bug-fix", "grc")
    assert len(plan["phases"]) == 9, (
        f"expected 9 phases (prepare, build-inputs, warm, launch, monitor, "
        f"collect, score, report, cleanup); got {len(plan['phases'])}: "
        f"{[ph['name'] for ph in plan['phases']]}"
    )
