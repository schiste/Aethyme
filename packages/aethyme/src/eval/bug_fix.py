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
import shutil
import sys
import threading
from pathlib import Path
from typing import TYPE_CHECKING, Any

from src.contracts.versions import contract_versions

from ._self import is_self_tool, self_tool_name

if TYPE_CHECKING:
    from .tools import ToolAdapter
from .bug_fix_setup import (
    CROSS_PACKAGE_TEST_REL,
    RBAC_REL,
    TEST_REL,
    create_cross_package_test,
    create_test_file,
    plant_bug,
    plant_cross_package_bug,
    reset_bug,
    verify_setup,
)
from .inline_warm import fanout_tool_state, warm_and_query_leverage
from .models import EvaluationSide
from .navigation_context import build_scope_view
from .report import (
    CONDITION_ORDER,
    EvaluationReport,
    estimate_report,
    finalize_eval_run,
    write_bug_fix_markdown_report,
)
from .runner import CommandEvaluationRunner, EvaluationRunner
from .schemas import bug_fix_output_schema, bug_fix_scoring_rubric
from .scoring import parse_structured_output, score_bug_fix_output

# Default auto-cleanup delay for benchmark clones. Bumped 2026-05-09 from
# 5 → 15 min after a 2026-05-07 GRC run lost its first clone dir mid-debug.
# 15 min covers the agent runtime (typically 1-3 min on Mockup, up to ~16
# min on MediaWiki) plus a generous buffer; eval runners that take longer
# should call `cancel_cleanup_timer()` on the result dict explicitly (which
# the launch phase orchestration does automatically).
_CLEANUP_DELAY_SECONDS = 15 * 60

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
        "contract_versions": contract_versions(),
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
    alternative_task: str | None = None,
    auto_cleanup: bool = True,
    cleanup_delay: float = _CLEANUP_DELAY_SECONDS,
    tool: ToolAdapter | None = None,
) -> dict[str, Any]:
    """Create one clone per condition, plant the bug, generate all artifacts.

    Each condition gets its own repo clone so agents can't contaminate each
    other (e.g. one fixing the bug before the next runs).

    When *alternative_task* is provided, also generates a plausibly-wrong
    nav-context for the negative-context condition (built by running the
    same generator on the same repo against the alternative task; the 5-
    property plausibility rule is enforced via
    ``generate_plausible_error_context``). When omitted, the negative-
    context condition still gets a clone but its nav-context file falls
    back to the real one — making the condition equivalent to leverage,
    which the orchestrator can detect via the artifacts dict.

    When *auto_cleanup* is True (default), schedules deletion of *dest_dir*
    after *cleanup_delay* seconds. The timer handle is returned under
    ``cleanup_timer`` — call ``.cancel()`` to keep the clones.

    Returns dict with repos, prompts, artifacts paths, and shared eval data.
    """
    from .repos import CONDITION_NAMES, create_condition_repos

    # 1. Create one independent clone per condition. When a tool adapter
    # is supplied, it's used to install the tool into the explore/
    # leverage/task-conditioned clones (replaces the legacy direct
    # deploy_skills call).
    repos = create_condition_repos(source, dest_dir, tool=tool)

    # 2. Plant bug + create test in every clone
    setup_results: dict[str, Any] = {}
    for cond, repo_path in repos.items():
        setup_results[cond] = setup_bug_fix(repo_path)

    # Tool dispatch — the framework's self-tool (the tool AethymeBench
    # treats as its primary subject — see :mod:`src.eval._self`) keeps
    # the structured-JSON nav-context flow (anchors / scope /
    # file_contents in navigation_context.json); other tools use a
    # tool-context-file pointer in the leverage prompt and skip the
    # self-tool-specific negative-context plausibility flow entirely.
    # The negative-context condition for non-self tools degenerates to
    # a leverage replay with ``negative_context_status`` reflecting
    # that it was auto-skipped — honest about not implementing a
    # trust-calibration condition that the tool's data shape doesn't
    # support.
    is_self = is_self_tool(tool.name if tool is not None else None)
    tool_context_relpath = ".aethyme-eval-tool-context.md"

    # 3. Build per-condition prompts (each embeds its own repo path).
    prompts: dict[str, str] = {}
    for cond, repo_path in repos.items():
        if cond == "leverage":
            prompts[cond] = _build_bug_fix_prompt(
                repo_path, task, condition="leverage",
                tool_name=(tool.name if tool is not None else None),
                tool_context_relpath=(
                    None if is_self else tool_context_relpath
                ),
            )
        elif cond == "negative-context":
            if is_self:
                prompts[cond] = _build_bug_fix_prompt(
                    repo_path, task, condition="negative-context",
                )
            else:
                # Auto-skip semantics for non-Aethyme tools: deliver the
                # same prompt as leverage so the agent still does
                # *some* work and the table row isn't empty, but record
                # the status as skipped so reports surface that no
                # trust-calibration measurement was made.
                prompts[cond] = _build_bug_fix_prompt(
                    repo_path, task, condition="leverage",
                    tool_name=(tool.name if tool is not None else None),
                    tool_context_relpath=tool_context_relpath,
                )
        else:
            prompts[cond] = _build_bug_fix_prompt(
                repo_path, task, condition="baseline",
            )

    # 4. Build navigation contexts.
    leverage_repo = repos["leverage"]
    navigation_context = _build_navigation_context(leverage_repo, task, tool=tool)

    # Tool-context file + per-clone graph state (non-Aethyme only).
    #
    # Two artifacts to wire per tool-using clone:
    #
    # 1. Tool-specific state (e.g. graphify-out/graph.json). All
    #    TOOL_USING_CONDITIONS clones need this — even explore and
    #    task-conditioned, where the agent might INVOKE the tool
    #    despite no prompt pointer. Without per-clone state, calls
    #    fail. We extract once (in leverage_repo, where we'd run the
    #    query anyway) and then copy graphify-out/ to the other
    #    tool-using clones. ~225MB per clone on Mockup; copy is fast
    #    on the same filesystem.
    #
    # 2. tool-context.md prompt addendum. ONLY in leverage and
    #    negative-context, because those are the conditions whose
    #    prompts explicitly point at the file. Explore and
    #    task-conditioned use baseline prompts (no pointer) — having
    #    the file there would be irrelevant since the prompt doesn't
    #    reference it.
    #
    # Per the bug_fix.py / explain_repo.py / navigation_ctf.py
    # pattern: failures in warm or run_condition are non-fatal. The
    # eval still runs; the agent just doesn't get the leverage
    # context.
    if not is_self and tool is not None:
        addendum = warm_and_query_leverage(
            tool, leverage_repo, task,
            tool_context_path=leverage_repo / tool_context_relpath,
            log_prefix="bug_fix.prepare",
        )

        # Fan the same addendum out to negative-context (its prompt
        # also points at tool-context.md). We don't re-query — same
        # addendum by design, and re-querying would double tool cost
        # and risk nondeterminism if the adapter isn't stable.
        negative_target = repos.get("negative-context")
        if negative_target is not None:
            (negative_target / tool_context_relpath).write_text(addendum, encoding="utf-8")

        # Per-clone tool state → explore, task-conditioned,
        # negative-context. Leverage already has its own (it's where
        # we warmed). Copy rather than re-warm: same rationale as
        # inline_warm.fanout_tool_state's docstring.
        fanout_targets = [
            repos[c] for c in ("explore", "task-conditioned", "negative-context")
            if c in repos
        ]
        fanout_tool_state(
            leverage_repo, fanout_targets,
            log_prefix="bug_fix.prepare",
        )

    # Negative nav-context (plausibly-wrong) — only meaningful for
    # Aethyme tooling (the 5-property plausibility rule operates on
    # Aethyme's anchor schema). For non-Aethyme tools, skip entirely
    # and surface that in the status; the negative-context condition
    # will execute as a leverage replay but the report knows it's not
    # a real trust-calibration measurement.
    negative_navigation_context: dict[str, Any] | None = navigation_context
    if not is_self:
        negative_status = "skipped_non_aethyme_tool"
    else:
        negative_status = "fallback_no_alternative"
        if alternative_task:
            negative_repo = repos.get("negative-context")
            if negative_repo is not None:
                try:
                    negative_navigation_context = generate_plausible_error_context(
                        navigation_context,
                        repo_path=negative_repo,
                        alternative_task=alternative_task,
                    )
                    negative_status = "ok"
                except PlausibilityError as exc:
                    negative_status = f"fallback_plausibility_failed: {exc}"

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

    # Aethyme-specific nav-context JSON artifacts. Non-Aethyme tools
    # use the tool-context file inside the leverage clone (written
    # above) and don't produce these JSON artifacts; the report will
    # surface ``navigation_context: null`` so downstream consumers can
    # see the condition was wired differently.
    if navigation_context is not None:
        nav_path = _LEVERAGE_NAV_CONTEXT_PATH
        Path(nav_path).write_text(json.dumps(navigation_context, indent=2))
        artifact_paths["navigation_context"] = nav_path

    if negative_navigation_context is not None:
        neg_nav_path = _NEGATIVE_NAV_CONTEXT_PATH
        Path(neg_nav_path).write_text(json.dumps(negative_navigation_context, indent=2))
        artifact_paths["negative_navigation_context"] = neg_nav_path

    result = {
        "repos": {cond: str(path) for cond, path in repos.items()},
        "setup": setup_results,
        "prompts": prompts,
        "artifacts": artifact_paths,
        "reference": reference,
        "output_schema": output_schema,
        "scoring_rubric": scoring_rubric,
        "navigation_context": navigation_context,
        "negative_navigation_context": negative_navigation_context,
        "negative_context_status": negative_status,
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
        # Route the heads-up to stderr, NOT stdout. Callers using
        # `--json-output | jq` need stdout to be valid JSON only;
        # the cleanup notice is operator-facing status, so stderr is
        # the natural channel. Pre-2026-05-09 this went to stdout and
        # broke every pipe consumer.
        print(
            f"Benchmark clones at {dest_dir} will be deleted in {mins:.0f} min.",
            file=sys.stderr,
        )

    return result


def cleanup_benchmark(dest_dir: Path) -> None:
    """Remove benchmark clones immediately."""
    dest_dir = Path(dest_dir).resolve()
    if dest_dir.exists():
        shutil.rmtree(dest_dir, ignore_errors=True)


def schedule_cleanup(dest_dir: Path, *, delay: float = _CLEANUP_DELAY_SECONDS) -> threading.Timer:
    """Schedule removal of benchmark clones after *delay* seconds (default 15 min).

    Returns the timer so callers can ``.cancel()`` it if they need the clones
    longer (e.g. for manual inspection).
    """
    dest_dir = Path(dest_dir).resolve()
    timer = threading.Timer(delay, cleanup_benchmark, args=[dest_dir])
    timer.daemon = True  # don't prevent process exit
    timer.start()
    return timer


def cancel_cleanup_timer(prepare_result: dict[str, Any]) -> bool:
    """Cancel the auto-cleanup timer if `prepare_bug_fix_benchmark` armed one.

    Returns True if a timer was cancelled, False otherwise (no timer set, or
    already fired). Idempotent — safe to call multiple times.

    Intended for the launch-phase orchestration: once agents have been
    dispatched to the clones, the run is committed and we no longer want
    them deleted out from under us.
    """
    timer = prepare_result.get("cleanup_timer")
    if timer is None:
        return False
    if not isinstance(timer, threading.Timer):
        return False
    timer.cancel()
    return True


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


_LEVERAGE_NAV_CONTEXT_PATH = "/tmp/aethyme-eval-navigation-context.json"
_NEGATIVE_NAV_CONTEXT_PATH = "/tmp/aethyme-eval-negative-navigation-context.json"

# Leverage's preamble is a *minimal pointer* at the deployed skill —
# no nav-context blob path, no per-task command examples, no intent
# names. The 2026-05-09 trim revealed that the prior blob-reference
# preamble inflated leverage cost via cache-create overhead without
# changing tool usage; the minimal pointer aligns leverage with the
# `_LEVERAGE_HINT` pattern in `prompts.py` and turns leverage into
# a pure discoverability+pointer test (vs explore's pure
# discoverability test).
_LEVERAGE_MINIMAL_POINTER = (
    "Aethyme is available in this repository. See "
    "`.codex/skills/aethyme/SKILL.md` for usage; the wrapper at "
    "`.codex/skills/aethyme/aethyme-explore` is the convenience "
    "entry point. Use `--depth 0` first to triage scope, escalate "
    "one rung at a time. Verify its output before acting on it.\n\n"
)


def _build_bug_fix_prompt(
    repo_path: Path,
    task: str,
    *,
    condition: str = "baseline",
    tool_name: str | None = None,
    tool_context_relpath: str | None = None,
) -> str:
    """Build the bug-fix prompt for one of three condition shapes.

    Body is identical across conditions — only the preamble varies:

    - ``"baseline"`` (control + explore + task-conditioned): no preamble.
      Agents either don't have the skill (control) or have it without
      explicit instruction (explore).
    - ``"leverage"``: pointer at the tool. For the framework's self-tool
      (Aethyme by default — see :mod:`src.eval._self`), uses the
      self-tool-specific minimal pointer at SKILL.md. For other tools,
      uses a generic pointer at the pre-computed tool-context file
      (``tool_context_relpath``, relative to the repo root).
    - ``"negative-context"``: pointer at the *plausibly-wrong*
      nav-context blob file (asymmetric with leverage by design —
      this condition tests trust calibration when the agent is
      handed wrong context upfront). Self-tool-specific.

    ``tool_name`` + ``tool_context_relpath`` are only meaningful for
    the leverage condition with a non-self tool. When ``tool_name`` is
    None or matches the framework's self-tool, the existing self-tool
    preamble is used — preserving byte-identical prompts for the
    default flow.
    """
    if condition == "leverage":
        is_other_tool = tool_name is not None and not is_self_tool(tool_name)
        if is_other_tool and tool_context_relpath:
            # Mirrors Aethyme's _LEVERAGE_MINIMAL_POINTER shape: a small
            # pointer hint that names the tool and tells the agent where
            # to find pre-computed context. The agent decides whether
            # to Read the file — same structural decision as the
            # Aethyme leverage condition, so comparison stays apples-
            # to-apples.
            preamble = (
                f"{tool_name} is available in this repository. Pre-computed "
                f"task-specific context is at `{tool_context_relpath}` "
                f"(produced by {tool_name}'s leverage command before this "
                f"session started). Read it if useful; verify its output "
                f"before acting on it.\n\n"
            )
        else:
            preamble = _LEVERAGE_MINIMAL_POINTER
    elif condition == "negative-context":
        preamble = (
            "Use Aethyme tools to navigate the repository graph.\n"
            f"Navigation context is available at {_NEGATIVE_NAV_CONTEXT_PATH}\n\n"
        )
    else:
        preamble = ""

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
    *,
    tool: ToolAdapter | None = None,
) -> dict[str, Any] | None:
    """Build navigation context for the leverage condition.

    When ``tool`` is ``None`` the framework's self-tool adapter is
    resolved via ``get_adapter(self_tool_name())`` — AethymeBench's
    default subject (overridable via ``AETHYMEBENCH_SELF_TOOL``; see
    :mod:`src.eval._self`). Routing everything through the adapter
    (subprocess + JSON parse) is the single source of truth: no direct
    Python calls into the indexing layer, no transport-dependent
    divergence to keep parity-tested.

    Returns the self-tool-shaped ``{anchors, scope, file_contents, ...}``
    dict when ``tool`` is the self-tool. Returns ``None`` for any other
    tool — non-self tools deliver leverage context via a separate
    file-pointer mechanism wired in :func:`prepare_bug_fix_benchmark`,
    so producing a synthetic self-tool-shape JSON would be misleading.

    Failure mode: subprocess or JSON-parse failures fall back to empty
    dicts so the prepare flow surfaces a degraded score rather than a
    hard error. The leverage condition's nav-context will be empty in
    that case.
    """
    if tool is None:
        from .tools import get_adapter

        tool = get_adapter(self_tool_name())
    if not is_self_tool(tool.name):
        return None

    try:
        leverage_result = tool.run_condition("leverage", repo_path, task)
        task_pack = json.loads(leverage_result.raw_output) if leverage_result.raw_output else {}
    except Exception:
        task_pack = {}

    try:
        tc_result = tool.run_condition("task-conditioned", repo_path, task)
        task_context = json.loads(tc_result.raw_output) if tc_result.raw_output else {}
    except Exception:
        task_context = {}

    scope_view = build_scope_view(task, task_pack)

    return {
        "mode": "bug_fix_navigation",
        "repo_path": str(repo_path),
        "task": task,
        "test_file": TEST_REL,
        "bug_area": "packages/auth/src/",
        "anchors": task_pack.get("anchors", []),
        "scope": scope_view,
        "file_contents": task_context.get("file_contents", []),
    }


# ===========================================================================
# Plausible-error generator (negative-context eval condition)
# ===========================================================================
#
# The negative-context condition tests *trust calibration* — what does the
# agent do when given context that LOOKS right but answers the wrong
# question? It isolates two effects the leverage measurement entangles:
#
# 1. Loading cost (tokens spent ingesting any context, right or wrong).
# 2. Misdirection cost (extra turns spent following bad anchors before
#    discovering the mismatch).
#
# A candidate negative context is "plausible" when it would survive the
# agent's surface-level skepticism — meaning the agent has to do real
# verification work to discover the mismatch. We generate it by running
# the SAME nav-context generator on the SAME repo with a DIFFERENT task,
# then enforce 5 properties so accidental easy-mismatches don't sneak in.
#
# This is generic across any repo / language / framework — the rule
# operates on the nav-context JSON shape, never on file contents or
# language-specific conventions.

DEFAULT_PLAUSIBILITY_OVERLAP_THRESHOLD = 0.30
"""How much of the real context's anchor set must also appear in the
candidate. Set per-direction (real ∩ cand / real), not Jaccard, so
small candidate sets aren't disproportionately punished. 0.30 is a
deliberate guess — too low and we admit topical-mismatch contexts as
'plausible', too high and we can't generate plausible candidates for
narrow modules. Tunable per-call via `overlap_threshold` argument."""

# Maximum allowed size ratio between candidate and real (in either
# direction). 2.0 = candidate may be at most 2× larger or 2× smaller
# than the real. Catches the "candidate is a tiny stub or a giant blob"
# failure modes that would let the agent dismiss it on size alone.
_MAX_SIZE_RATIO = 2.0


class PlausibilityError(ValueError):
    """Raised when a candidate negative context fails the plausibility rule.

    Distinct from generic ValueError so orchestrators can choose to
    downgrade (skip the negative-context condition this run) rather than
    fail the entire eval. The error message names the violated property
    and the offending values so the caller can adjust the alternative
    task without re-deriving from scratch.
    """


def _anchor_identifier_set(ctx: dict[str, Any]) -> set[str]:
    """Extract the stable identifier set from a nav-context.

    Repo-agnostic by design: pulls anchor `id` fields rather than file
    paths or content tokens. Anchor IDs are symbol-level identifiers
    produced by the engine, so the same comparison shape works for
    TypeScript, PHP, Rust, etc. Anchors without an `id` are skipped
    rather than substituted — they don't contribute signal.
    """
    out: set[str] = set()
    for anchor in ctx.get("anchors", []) or []:
        if isinstance(anchor, dict):
            anchor_id = anchor.get("id")
            if isinstance(anchor_id, str) and anchor_id:
                out.add(anchor_id)
    return out


def _assert_plausibly_wrong(
    real: dict[str, Any],
    candidate: dict[str, Any],
    *,
    overlap_threshold: float,
) -> None:
    """Enforce the 5-property plausibility rule. Raise PlausibilityError
    on the first violation, naming the property and offending values.

    Properties (in order checked):

      1. Shape-identical    — same top-level keys.
      2. Approximate-size   — within `_MAX_SIZE_RATIO` (currently 2×).
      3. Domain-adjacent    — same `repo_path`. Cross-repo candidates
                              fail trivially as topical mismatch.
      4. Identifier-overlap — fraction of real anchor IDs that also
                              appear in candidate ≥ overlap_threshold.
      5. Distinct           — anchor sets must NOT be identical, else
                              the candidate is the same context, not
                              a *wrong* one.

    Property 6 from the design doc ("real, not synthetic") is satisfied
    by construction when the candidate is built via the production
    nav-context generator — we don't runtime-check it, we just require
    callers to use `generate_plausible_error_context()`.
    """
    # 1. Shape
    if set(real.keys()) != set(candidate.keys()):
        only_real = set(real.keys()) - set(candidate.keys())
        only_cand = set(candidate.keys()) - set(real.keys())
        raise PlausibilityError(
            "shape mismatch: real has unique keys "
            f"{sorted(only_real)}, candidate has unique keys "
            f"{sorted(only_cand)}"
        )

    # 2. Approximate size
    real_size = len(json.dumps(real))
    cand_size = len(json.dumps(candidate))
    if real_size == 0 or cand_size == 0:
        raise PlausibilityError(
            f"degenerate sizes: real={real_size}, cand={cand_size}"
        )
    ratio = cand_size / real_size
    if ratio < (1 / _MAX_SIZE_RATIO) or ratio > _MAX_SIZE_RATIO:
        raise PlausibilityError(
            f"size ratio {ratio:.2f}× outside [1/{_MAX_SIZE_RATIO}, "
            f"{_MAX_SIZE_RATIO}]: real={real_size}B, cand={cand_size}B"
        )

    # 3. Domain (same repo)
    if real.get("repo_path") != candidate.get("repo_path"):
        raise PlausibilityError(
            f"different repos: real={real.get('repo_path')!r}, "
            f"cand={candidate.get('repo_path')!r}"
        )

    # 4. Identifier overlap
    real_ids = _anchor_identifier_set(real)
    cand_ids = _anchor_identifier_set(candidate)
    if not real_ids or not cand_ids:
        raise PlausibilityError(
            f"empty anchor id set: real={len(real_ids)}, cand={len(cand_ids)}"
        )
    overlap_fraction = len(real_ids & cand_ids) / len(real_ids)
    if overlap_fraction < overlap_threshold:
        raise PlausibilityError(
            f"identifier overlap {overlap_fraction:.2f} below threshold "
            f"{overlap_threshold:.2f}: only "
            f"{len(real_ids & cand_ids)} of {len(real_ids)} real anchors "
            "appear in candidate"
        )

    # 5. Distinct
    if real_ids == cand_ids:
        raise PlausibilityError(
            "candidate anchor set identical to real — not a *wrong* "
            "context, just the same context. Use a more divergent "
            "alternative task."
        )


def generate_plausible_error_context(
    real_context: dict[str, Any],
    *,
    repo_path: Path,
    alternative_task: str,
    overlap_threshold: float = DEFAULT_PLAUSIBILITY_OVERLAP_THRESHOLD,
) -> dict[str, Any]:
    """Generate a plausibly-wrong nav-context for the negative-context
    eval condition.

    The candidate is produced by running the production nav-context
    generator on the same repo with a different task. Because the
    generator is the same, the candidate is a *real* Aethyme artifact
    — not synthetic — that just answers the wrong question.

    Parameters
    ----------
    real_context:
        The production nav-context that the leverage condition uses.
        Used as the reference for shape/size/identifier comparisons.
    repo_path:
        Same repo as the real context. Required because the candidate
        must satisfy domain-adjacency.
    alternative_task:
        A different task description for the same repo. Caller's
        responsibility to choose an alternative whose anchors plausibly
        overlap the real context (typically: a sibling task in the
        same module). The function is repo-agnostic — it doesn't
        synthesize the alternative, only consumes it.
    overlap_threshold:
        Minimum fraction of real anchor IDs that must also appear in
        the candidate (default 0.30). Lower threshold = looser
        plausibility; higher threshold = stricter.

    Returns
    -------
    dict:
        The candidate nav-context. Same shape as the real one,
        guaranteed to satisfy the 5-property plausibility rule.

    Raises
    ------
    PlausibilityError:
        If the candidate fails any of the 5 properties. Caller can
        catch this and either pick a different `alternative_task` or
        skip the negative-context condition for this run.
    """
    candidate = _build_navigation_context(repo_path, alternative_task)
    _assert_plausibly_wrong(
        real_context, candidate, overlap_threshold=overlap_threshold
    )
    return candidate


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
    from .repos import CONDITION_NAMES, TOOL_USING_CONDITIONS, create_condition_repos

    repos = create_condition_repos(source, dest_dir)

    setup_results: dict[str, Any] = {}
    for cond, repo_path in repos.items():
        setup_results[cond] = setup_cross_package_bug_fix(repo_path)

    prompts: dict[str, str] = {}
    for cond, repo_path in repos.items():
        leverage = cond in TOOL_USING_CONDITIONS and cond == "leverage"
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
