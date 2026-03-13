"""Bug-fix artifact generation for local Aethyme benchmarks.

Generates prompts, schemas, reference outputs, and navigation context for the
bug-fix benchmark.  Scoring is primarily objective: vitest pass/fail determines
80% of the score.  Actual eval execution is done externally by launching agent
sessions via Chau7 MCP or equivalent.

Unlike explain-repo and navigation-ctf, the bug-fix eval's reference is static
(hardcoded ground truth) and scoring depends on running vitest in the agent's
workspace *after* the agent finishes — not on comparing structured output.
"""

from __future__ import annotations

import json
import shlex
import shutil
import threading
from pathlib import Path
from typing import Any

from ..indexing.engine import build_task_context, build_task_pack
from .models import EvaluationSide
from .bug_fix_setup import (
    CROSS_PACKAGE_TEST_REL,
    RBAC_REL,
    TEST_REL,
    create_cross_package_test,
    create_test_file,
    plant_bug,
    plant_cross_package_bug,
    reset_bug,
    reset_cross_package_bug,
    run_cross_package_fix_test,
    run_cross_package_regression_tests,
    run_fix_test,
    run_regression_tests,
    verify_setup,
)
from .control_prompt import build_baseline_prompt, build_leverage_prompt
from .report import (
    CONDITION_ORDER,
    EvaluationReport,
    create_eval_run_dir,
    estimate_report,
    finalize_eval_run,
    write_bug_fix_markdown_report,
)
from .runner import CommandEvaluationRunner, EVAL_TOOL_PYTHON, EvaluationRunner, PROJECT_ROOT
from .schemas import bug_fix_output_schema, bug_fix_scoring_rubric
from .scoring import parse_structured_output, score_bug_fix_output

_CLEANUP_DELAY_SECONDS = 5 * 60  # 5 minutes

DEFAULT_TASK = "Fix failing test: manage permission does not imply share in ability-implications.test.ts"
DEFAULT_CROSS_PACKAGE_TASK = "Fix regression: execute permission does not grant read access"

# The prompt includes simulated test failure output so agents don't need to
# discover the failure themselves — they can jump straight to root-cause analysis.
_TEST_FAILURE_OUTPUT = """\
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true
"""

# Cross-package scenario: symptom-driven failure output with NO file paths.
# This forces agents to search across the codebase to find the failing test
# and trace the dependency chain to the root cause.
_CROSS_PACKAGE_FAILURE_OUTPUT = """\
4 tests failed
  x execute permission should grant read access on integrations
    -> expected true to be false
  x execute:integrations grants read as implied sub-permission
    -> read check failed
  x canAll returns true for execute with read check
    -> expected true to be false
  x canAny resolves execute-implied read across helpers
    -> expected true to be false
"""


def run_bug_fix_evaluation(
    repo_path: Path,
    task: str = DEFAULT_TASK,
    control_runner: EvaluationRunner | None = None,
    explore_runner: EvaluationRunner | None = None,
    leverage_runner: EvaluationRunner | None = None,
) -> dict[str, Any]:
    """Generate all eval artifacts for a bug-fix evaluation.

    When runners are ``None`` (the default), this generates prompts, schema,
    reference, and navigation context without executing agents.  Artifacts
    are written to /tmp/ for use with Chau7 MCP.

    Returns dict with prompts, output_schema, reference, navigation_context,
    test_commands, and (if runners were provided) condition results.
    """
    reference = _build_bug_fix_reference()
    output_schema = bug_fix_output_schema()
    navigation_context = _build_navigation_context(repo_path, task)

    # --- Build prompts ---
    control_prompt = _build_bug_fix_prompt(repo_path, task)
    explore_prompt = _build_bug_fix_prompt(repo_path, task)
    leverage_prompt = _build_bug_fix_prompt(repo_path, task, leverage=True)

    # --- Execute conditions via command backends (if provided) ---
    control_run = (
        control_runner.run(
            label="control",
            prompt=control_prompt,
            repo_path=repo_path,
            task=task,
            output_schema=output_schema,
        )
        if control_runner
        else None
    )
    explore_run = (
        explore_runner.run(
            label="explore",
            prompt=explore_prompt,
            repo_path=repo_path,
            task=task,
            navigation_context=None,
            output_schema=output_schema,
        )
        if explore_runner
        else None
    )
    leverage_run = (
        leverage_runner.run(
            label="leverage",
            prompt=leverage_prompt,
            repo_path=repo_path,
            task=task,
            navigation_context=navigation_context,
            output_schema=output_schema,
        )
        if leverage_runner
        else None
    )

    control_candidate = parse_structured_output(control_run.stdout) if control_run else None
    explore_candidate = parse_structured_output(explore_run.stdout) if explore_run else None
    leverage_candidate = parse_structured_output(leverage_run.stdout) if leverage_run else None

    prompts = {"control": control_prompt, "explore": explore_prompt, "leverage": leverage_prompt}
    runs = {"control": control_run, "explore": explore_run, "leverage": leverage_run}

    report: EvaluationReport = estimate_report(task, prompts=prompts, runs=runs)

    # --- Scoring (reference used here only) ---
    # Note: test_pass and regression_pass default to False since we can't
    # run vitest from this process — that happens in the agent's workspace
    # via Chau7 MCP after the agent finishes.
    control_assessment = score_bug_fix_output(control_candidate, reference, repo_path=str(repo_path))
    explore_assessment = score_bug_fix_output(explore_candidate, reference, repo_path=str(repo_path))
    leverage_assessment = score_bug_fix_output(leverage_candidate, reference, repo_path=str(repo_path))

    result: dict[str, Any] = {
        "task": task,
        "eval_type": "bug-fix",
        "reference": reference,
        "control": EvaluationSide(
            prompt=control_prompt,
            run=control_run.to_dict() if control_run else None,
            assessment=control_assessment,
        ).to_dict(),
        "explore": EvaluationSide(
            prompt=explore_prompt,
            run=explore_run.to_dict() if explore_run else None,
            assessment=explore_assessment,
        ).to_dict(),
        "leverage": EvaluationSide(
            prompt=leverage_prompt,
            run=leverage_run.to_dict() if leverage_run else None,
            assessment=leverage_assessment,
        ).to_dict(),
        "navigation_context": navigation_context,
        "output_schema": output_schema,
        "scoring_rubric": bug_fix_scoring_rubric(),
        "reference_output": reference,
        "test_commands": {
            "fix_test": f"npx vitest run {TEST_REL}",
            "regression_test": "npx vitest run packages/auth/src/__tests__/",
            "reset": f"git checkout HEAD -- {RBAC_REL}",
        },
        "report": report.to_dict(),
    }
    report_path = write_bug_fix_markdown_report(repo_path=repo_path, result=result)
    result["report_path"] = str(report_path)
    return result


def setup_bug_fix(repo_path: Path) -> dict[str, Any]:
    """Plant the bug and create the test file in a Playground repo.

    Returns a diagnostic dict with plant status and test file path.
    """
    plant_result = plant_bug(repo_path)
    test_path = create_test_file(repo_path)
    return {
        "plant": plant_result,
        "test_file": str(test_path),
        "repo_path": str(repo_path),
    }


def verify_bug_fix_setup(repo_path: Path) -> dict[str, object]:
    """Verify the bug is planted correctly: planted test fails, existing tests pass."""
    return verify_setup(repo_path)


def reset_bug_fix(repo_path: Path) -> None:
    """Restore rbac-canonical.ts to its committed state."""
    reset_bug(repo_path)


def prepare_bug_fix_benchmark(
    source: Path,
    dest_dir: Path,
    task: str = DEFAULT_TASK,
    *,
    auto_cleanup: bool = True,
    cleanup_delay: float = _CLEANUP_DELAY_SECONDS,
) -> dict[str, Any]:
    """Create 4 isolated clones, plant the bug in each, and generate all artifacts.

    Each condition gets its own repo clone so agents can't contaminate each
    other (e.g. one fixing the bug before the next runs).

    When *auto_cleanup* is True (default), schedules deletion of *dest_dir*
    after *cleanup_delay* seconds (default 5 min).  The timer handle is
    returned under ``cleanup_timer`` — call ``.cancel()`` to keep the clones.

    Returns dict with repos, prompts, artifacts paths, and shared eval data.
    """
    from .repos import create_condition_repos, CONDITION_NAMES, AETHYME_CONDITIONS

    # 1. Create 4 independent clones
    repos = create_condition_repos(source, dest_dir)

    # 2. Plant bug + create test in every clone
    setup_results: dict[str, Any] = {}
    for cond, repo_path in repos.items():
        setup_results[cond] = setup_bug_fix(repo_path)

    # 3. Build per-condition prompts (each embeds its own repo path)
    prompts: dict[str, str] = {}
    for cond, repo_path in repos.items():
        leverage = cond in AETHYME_CONDITIONS and cond == "leverage"
        prompts[cond] = _build_bug_fix_prompt(repo_path, task, leverage=leverage)

    # 4. Build navigation context from the leverage clone
    leverage_repo = repos["leverage"]
    navigation_context = _build_navigation_context(leverage_repo, task)

    # 5. Shared artifacts
    reference = _build_bug_fix_reference()
    output_schema = bug_fix_output_schema()
    scoring_rubric = bug_fix_scoring_rubric()

    # 6. Write all artifacts to /tmp/
    artifact_paths: dict[str, str] = {}
    for cond in CONDITION_NAMES:
        prompt_path = f"/tmp/aethyme-eval-{cond}-prompt.txt"
        Path(prompt_path).write_text(prompts[cond])
        artifact_paths[f"{cond}_prompt"] = prompt_path

    schema_path = "/tmp/aethyme-eval-output-schema.json"
    Path(schema_path).write_text(json.dumps(output_schema, indent=2))
    artifact_paths["output_schema"] = schema_path

    ref_path = "/tmp/aethyme-eval-reference.json"
    Path(ref_path).write_text(json.dumps(reference, indent=2))
    artifact_paths["reference"] = ref_path

    rubric_path = "/tmp/aethyme-eval-scoring-rubric.json"
    Path(rubric_path).write_text(json.dumps(scoring_rubric, indent=2))
    artifact_paths["scoring_rubric"] = rubric_path

    nav_path = "/tmp/aethyme-eval-navigation-context.json"
    Path(nav_path).write_text(json.dumps(navigation_context, indent=2))
    artifact_paths["navigation_context"] = nav_path

    result = {
        "repos": {cond: str(path) for cond, path in repos.items()},
        "setup": setup_results,
        "prompts": prompts,
        "artifacts": artifact_paths,
        "reference": reference,
        "output_schema": output_schema,
        "scoring_rubric": scoring_rubric,
        "navigation_context": navigation_context,
        "test_commands": {
            "fix_test": f"npx vitest run {TEST_REL}",
            "regression_test": "npx vitest run packages/auth/src/__tests__/",
        },
        "dest_dir": str(dest_dir),
    }

    if auto_cleanup:
        timer = schedule_cleanup(dest_dir, delay=cleanup_delay)
        result["cleanup_timer"] = timer
        mins = cleanup_delay / 60
        print(f"Benchmark clones at {dest_dir} will be deleted in {mins:.0f} min.")

    return result


def cleanup_benchmark(dest_dir: Path) -> None:
    """Remove benchmark clones immediately."""
    dest_dir = Path(dest_dir).resolve()
    if dest_dir.exists():
        shutil.rmtree(dest_dir, ignore_errors=True)


def schedule_cleanup(dest_dir: Path, *, delay: float = _CLEANUP_DELAY_SECONDS) -> threading.Timer:
    """Schedule removal of benchmark clones after *delay* seconds (default 5 min).

    Returns the timer so callers can ``.cancel()`` it if they need the clones
    longer (e.g. for manual inspection).
    """
    dest_dir = Path(dest_dir).resolve()
    timer = threading.Timer(delay, cleanup_benchmark, args=[dest_dir])
    timer.daemon = True  # don't prevent process exit
    timer.start()
    return timer


def command_runner(command: str, working_directory: Path | None = None) -> CommandEvaluationRunner:
    return CommandEvaluationRunner(command=command, working_directory=working_directory)


def _build_bug_fix_reference() -> dict[str, Any]:
    """Static ground truth for scoring.

    Unlike explain-repo/navigation-ctf where references are derived from
    graph inspection, the bug-fix reference is hardcoded — the bug and fix
    are deterministic.
    """
    return {
        "bug_file": RBAC_REL,
        "bug_line": "    Action.SHARE,",
        "fix": "Add Action.SHARE back to PERMISSION_IMPLICATIONS[manage] array",
        "root_cause": "Action.SHARE was removed from the manage permission implications",
        "fix_applied": True,
        "fix_description": "Restored Action.SHARE to the PERMISSION_IMPLICATIONS[manage] array in rbac-canonical.ts",
    }


def _build_bug_fix_prompt(repo_path: Path, task: str, *, leverage: bool = False) -> str:
    """Build the bug-fix prompt for control/explore or leverage conditions.

    All conditions get the same core prompt with test failure information.
    Leverage additionally gets a nudge to use Aethyme tools.
    """
    preamble = (
        "Use Aethyme tools to navigate the repository graph.\n"
        "Navigation context is available at /tmp/aethyme-eval-navigation-context.json\n\n"
    ) if leverage else ""

    return (
        f"{preamble}"
        f"A test is failing in this repository.\n\n"
        f"Repository path: {repo_path}\n"
        f"Test file: {TEST_REL}\n"
        f"Run command: npx vitest run {TEST_REL}\n\n"
        f"Failure output:\n"
        f"{_TEST_FAILURE_OUTPUT}\n"
        f"The test file is correct. The bug is in the source code.\n"
        f"Find the root cause and fix the bug so all tests pass.\n"
        f"You can verify your fix by running the test command above."
    )


def _build_navigation_context(
    repo_path: Path,
    task: str,
) -> dict[str, Any]:
    """Build navigation context for the leverage condition.

    Uses the Aethyme engine to compute task-relevant context (anchors,
    scope, file contents) so the leverage agent starts with a map of
    the relevant code.
    """
    try:
        task_pack = build_task_pack(repo_path, task)
    except Exception:
        task_pack = {}

    try:
        task_context = build_task_context(repo_path, task, content_budget=40_000)
    except Exception:
        task_context = {}

    in_scope = task_pack.get("in_scope", {})
    out_of_scope = task_pack.get("out_of_scope", {})
    scope_view = {
        "task": task,
        "navigation_order": task_pack.get("navigation_order", []),
        "in_scope_files": [item["value"] for item in in_scope.get("files", [])],
        "in_scope_symbols": [item["value"] for item in in_scope.get("symbols", [])],
        "in_scope_areas": [item["value"] for item in in_scope.get("areas", [])],
        "out_of_scope": [item["value"] for item in out_of_scope.get("areas", [])],
    }

    return {
        "mode": "bug_fix_navigation",
        "repo_path": str(repo_path),
        "task": task,
        "test_file": TEST_REL,
        "bug_area": "packages/auth/src/",
        "anchors": task_pack.get("anchors", []),
        "scope": scope_view,
        "file_contents": task_context.get("files", {}),
    }


# =========================================================================
# Cross-package scenario
# =========================================================================


def setup_cross_package_bug_fix(repo_path: Path) -> dict[str, Any]:
    """Plant the cross-package bug and create the test in app-shared."""
    plant_result = plant_cross_package_bug(repo_path)
    test_path = create_cross_package_test(repo_path)
    return {
        "plant": plant_result,
        "test_file": str(test_path),
        "repo_path": str(repo_path),
    }


def prepare_cross_package_benchmark(
    source: Path,
    dest_dir: Path,
    task: str = DEFAULT_CROSS_PACKAGE_TASK,
) -> dict[str, Any]:
    """Create 4 isolated clones with the cross-package bug scenario.

    Same structure as ``prepare_bug_fix_benchmark`` but uses the
    symptom-driven prompt (no file paths) and plants the execute bug
    with the test in app-shared instead of auth.
    """
    from .repos import create_condition_repos, CONDITION_NAMES, AETHYME_CONDITIONS

    repos = create_condition_repos(source, dest_dir)

    setup_results: dict[str, Any] = {}
    for cond, repo_path in repos.items():
        setup_results[cond] = setup_cross_package_bug_fix(repo_path)

    prompts: dict[str, str] = {}
    for cond, repo_path in repos.items():
        leverage = cond in AETHYME_CONDITIONS and cond == "leverage"
        prompts[cond] = _build_cross_package_prompt(repo_path, task, leverage=leverage)

    leverage_repo = repos["leverage"]
    navigation_context = _build_navigation_context(leverage_repo, task)

    reference = _build_cross_package_reference()
    output_schema = bug_fix_output_schema()
    scoring_rubric = bug_fix_scoring_rubric()

    artifact_paths: dict[str, str] = {}
    for cond in CONDITION_NAMES:
        prompt_path = f"/tmp/aethyme-eval-{cond}-prompt.txt"
        Path(prompt_path).write_text(prompts[cond])
        artifact_paths[f"{cond}_prompt"] = prompt_path

    schema_path = "/tmp/aethyme-eval-output-schema.json"
    Path(schema_path).write_text(json.dumps(output_schema, indent=2))
    artifact_paths["output_schema"] = schema_path

    ref_path = "/tmp/aethyme-eval-reference.json"
    Path(ref_path).write_text(json.dumps(reference, indent=2))
    artifact_paths["reference"] = ref_path

    rubric_path = "/tmp/aethyme-eval-scoring-rubric.json"
    Path(rubric_path).write_text(json.dumps(scoring_rubric, indent=2))
    artifact_paths["scoring_rubric"] = rubric_path

    nav_path = "/tmp/aethyme-eval-navigation-context.json"
    Path(nav_path).write_text(json.dumps(navigation_context, indent=2))
    artifact_paths["navigation_context"] = nav_path

    return {
        "repos": {cond: str(path) for cond, path in repos.items()},
        "setup": setup_results,
        "prompts": prompts,
        "artifacts": artifact_paths,
        "reference": reference,
        "output_schema": output_schema,
        "scoring_rubric": scoring_rubric,
        "navigation_context": navigation_context,
        "test_commands": {
            "fix_test": f"npx vitest run {CROSS_PACKAGE_TEST_REL}",
            "regression_test": (
                "npx vitest run packages/auth/src/__tests__/ "
                f"&& npx vitest run {CROSS_PACKAGE_TEST_REL}"
            ),
        },
    }


def _build_cross_package_reference() -> dict[str, Any]:
    """Static ground truth for the cross-package scenario."""
    return {
        "bug_file": RBAC_REL,
        "bug_line": "  [Action.EXECUTE]: [Action.READ],",
        "fix": "Restore Action.READ to PERMISSION_IMPLICATIONS[execute] array",
        "root_cause": "Action.READ was removed from the execute permission implications",
        "fix_applied": True,
        "fix_description": (
            "Restored [Action.READ] to PERMISSION_IMPLICATIONS[Action.EXECUTE] "
            "in rbac-canonical.ts (was changed to [])"
        ),
    }


def _build_cross_package_prompt(
    repo_path: Path, task: str, *, leverage: bool = False
) -> str:
    """Symptom-driven prompt — no file paths, only behavioral description.

    The prompt deliberately omits:
    - The test file path
    - The package where the bug lives
    - Any file paths in the failure output

    This forces agents to search across the codebase, which is where
    navigation tools should provide an efficiency advantage.
    """
    preamble = (
        "Use Aethyme tools to navigate the repository graph.\n"
        "Navigation context is available at /tmp/aethyme-eval-navigation-context.json\n\n"
    ) if leverage else ""

    return (
        f"{preamble}"
        f"Bug report: Users with 'execute' permission on Integrations cannot "
        f"view integration details. The execute action should imply read "
        f"access, but the read check is failing. A regression test has been "
        f"committed and is failing.\n\n"
        f"Repository path: {repo_path}\n"
        f"Run tests: npx vitest run\n\n"
        f"Failure summary:\n"
        f"{_CROSS_PACKAGE_FAILURE_OUTPUT}\n"
        f"The test file is correct. The bug is in the source code.\n"
        f"Find the root cause and fix the bug so all tests pass."
    )


# =========================================================================
# Result assembly — bridge from Chau7 raw data to standard report pipeline
# =========================================================================


def assemble_bug_fix_result(
    conditions: dict[str, dict[str, Any]],
    *,
    task: str,
    repo_path: Path,
    scenario: str = "implication-share",
    reference: dict[str, Any] | None = None,
    output_schema: dict[str, Any] | None = None,
    navigation_context: dict[str, Any] | None = None,
    model: dict[str, Any] | None = None,
    notes: str = "",
) -> dict[str, Any]:
    """Assemble raw Chau7 eval data into the canonical result dict.

    This is the **only** function that should produce bug-fix result dicts.
    Never hand-assemble result dicts — always call this function.

    After calling this, pass the result to ``write_bug_fix_markdown_report()``
    or ``finalize_eval_run()`` — both produce the same standardized report.

    Parameters
    ----------
    conditions:
        Dict mapping condition names to raw data dicts.  Each dict should
        contain the keys collected during Chau7 orchestration::

            {
                "prompt": str,                    # prompt text
                "structured_output": dict | None, # agent's JSON result
                "test_pass": bool,                # planted test passes
                "regression_pass": bool,          # existing tests pass
                "cost_usd": float,                # total cost — primary efficiency metric
                "tokens": int,                    # total tokens (stored, not scored)
                "input_tokens": int | None,       # optional: input only
                "output_tokens": int | None,      # optional: output only
                "command": str,                   # agent launch command
                "exit_code": int,                 # 0 = success
                "duration_seconds": float | None, # wall time
                "tool_calls": list | None,        # from chau7 telemetry
                "stdout": str,                    # raw stdout
                "stderr": str,                    # raw stderr
                "final_output_message": str,      # agent's final message
            }

        Missing keys are filled with safe defaults.

    task:
        Human-readable task description (report header).
    repo_path:
        Base repo path (used for path normalization in scoring).
    scenario:
        ``"implication-share"`` or ``"cross-package"`` — selects scoring
        weights and reference if *reference* is None.
    reference:
        Ground truth dict.  If None, auto-selected from *scenario*.
    output_schema:
        JSON schema dict for the structured output format.
    navigation_context:
        Navigation context dict (for the leverage condition).
    model:
        Model metadata dict with keys like ``name``, ``provider``,
        ``reasoning``, ``backend``.  Example::

            {"name": "claude-haiku-4-5-20251001", "provider": "anthropic",
             "backend": "claude-code", "reasoning": "default"}

    notes:
        Free-text notes to include in the result dict.

    Returns
    -------
    dict:
        Canonical result dict ready for ``write_bug_fix_markdown_report()``
        or ``finalize_eval_run()``.
    """
    if reference is None:
        reference = (
            _build_cross_package_reference() if scenario == "cross-package"
            else _build_bug_fix_reference()
        )
    if output_schema is None:
        output_schema = bug_fix_output_schema()

    scoring_rubric = bug_fix_scoring_rubric()
    active_conditions = tuple(
        c for c in CONDITION_ORDER if c in conditions
    )

    # --- Build per-condition sides ---
    condition_prompt_chars: dict[str, int] = {}
    condition_runs: dict[str, dict[str, Any] | None] = {}

    result: dict[str, Any] = {
        "task": task,
        "eval_type": "bug-fix",
        "scenario": scenario,
        "reference": reference,
        "output_schema": output_schema,
        "scoring_rubric": scoring_rubric,
        "reference_output": reference,
        "navigation_context": navigation_context,
    }
    if model:
        result["model"] = model
    if notes:
        result["notes"] = notes

    for cond in active_conditions:
        raw = conditions[cond]
        prompt = raw.get("prompt", "")

        # Resolve token split
        input_tokens = raw.get("input_tokens")
        output_tokens = raw.get("output_tokens")
        total_tokens = raw.get("tokens", 0)
        if input_tokens is None and output_tokens is None and total_tokens > 0:
            input_tokens = total_tokens
            output_tokens = 0

        # Cost in USD — the universal efficiency metric
        cost_usd = raw.get("cost_usd", 0.0)

        # Build run dict
        run_dict: dict[str, Any] = {
            "label": cond,
            "command": raw.get("command", ""),
            "exit_code": raw.get("exit_code", 0),
            "duration_seconds": raw.get("duration_seconds", 0.0),
            "num_turns": raw.get("num_turns", 0),
            "cost_usd": cost_usd,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": raw.get("cache_read_tokens", 0),
            "cache_create_tokens": raw.get("cache_create_tokens", 0),
            "stdout": raw.get("stdout", ""),
            "stderr": raw.get("stderr", ""),
            "retries": raw.get("retries", 0),
            "review_burden": raw.get("review_burden", 0),
            "final_output_message": raw.get("final_output_message", ""),
            "structured_output": raw.get("structured_output"),
            "tool_calls": raw.get("tool_calls"),
        }

        # Score this condition
        candidate = raw.get("structured_output")
        repo_str = str(repo_path.resolve()) if repo_path else None
        assessment = score_bug_fix_output(
            candidate,
            reference,
            test_pass=raw.get("test_pass", False),
            regression_pass=raw.get("regression_pass", False),
            cost_usd=cost_usd,
            tokens_used=(input_tokens or 0) + (output_tokens or 0),
            repo_path=repo_str,
            scenario=scenario,
        )

        # Assemble EvaluationSide
        side = EvaluationSide(
            prompt=prompt,
            run=run_dict,
            assessment=assessment,
        )
        result[cond] = side.to_dict()

        condition_prompt_chars[cond] = len(prompt)
        condition_runs[cond] = run_dict

    # --- Build report metadata ---
    result["report"] = {
        "task": task,
        "repo_path": str(repo_path),
        "condition_prompt_chars": condition_prompt_chars,
        "navigation_items": 0,
        "risk_items": 0,
        "condition_runs": condition_runs,
    }

    return result


def write_bug_fix_report(
    conditions: dict[str, dict[str, Any]],
    *,
    task: str,
    repo_path: Path,
    scenario: str = "implication-share",
    reference: dict[str, Any] | None = None,
    output_schema: dict[str, Any] | None = None,
    navigation_context: dict[str, Any] | None = None,
    run_dir: Path | None = None,
    model: str | None = None,
    notes: str = "",
) -> Path:
    """One-call convenience: assemble raw data and write the standard report.

    Combines ``assemble_bug_fix_result()`` + ``write_bug_fix_markdown_report()``
    (or ``finalize_eval_run()`` if *run_dir* is provided).

    Returns the path to the written report.
    """
    result = assemble_bug_fix_result(
        conditions,
        task=task,
        repo_path=repo_path,
        scenario=scenario,
        reference=reference,
        output_schema=output_schema,
        navigation_context=navigation_context,
        model=model,
        notes=notes,
    )

    if run_dir is not None:
        finalize_eval_run(run_dir, result, repo_path=repo_path, eval_type="bug-fix")
        return run_dir / "report.md"

    return write_bug_fix_markdown_report(
        repo_path=repo_path,
        result=result,
    )
