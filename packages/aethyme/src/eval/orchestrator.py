"""Eval orchestrator — generates deterministic run plans for Chau7 MCP.

``generate_run_plan()`` is a **pure function**: no side effects, no Chau7 calls,
no file I/O.  It returns a structured dict with 8 phases that the orchestrating
agent (Claude) reads and runs mechanically via Chau7 MCP tools.

Usage::

    plan = generate_run_plan(eval_type="bug-fix", target="grc", model="haiku")
    # Claude reads plan["phases"] and runs each step via MCP
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from ._self import is_self_tool
from .report import get_aethyme_commit
from .runner import PROJECT_ROOT
from .targets import EvalTarget, get_target

# ---------------------------------------------------------------------------
# Model configuration
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ModelConfig:
    """Backend configuration for an agent session."""

    name: str
    provider: str  # "anthropic" | "openai"
    backend: str  # "claude" | "codex"
    backend_args: tuple[str, ...]
    input_cost_per_m: float = 0.0  # $/1M input tokens
    output_cost_per_m: float = 0.0  # $/1M output tokens
    reasoning: str = "default"

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "provider": self.provider,
            "backend": self.backend,
            "reasoning": self.reasoning,
            "input_cost_per_m": self.input_cost_per_m,
            "output_cost_per_m": self.output_cost_per_m,
        }


MODELS: dict[str, ModelConfig] = {
    "haiku": ModelConfig(
        "haiku", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
        input_cost_per_m=0.80,
        output_cost_per_m=4.00,
    ),
    "sonnet": ModelConfig(
        "sonnet", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
        input_cost_per_m=3.00,
        output_cost_per_m=15.00,
    ),
    "opus": ModelConfig(
        "opus", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
        input_cost_per_m=15.00,
        output_cost_per_m=75.00,
    ),
    "gpt-5.4": ModelConfig(
        "gpt-5.4", "openai", "codex",
        ("--sandbox", "workspace-write"),
        input_cost_per_m=2.00,
        output_cost_per_m=8.00,
    ),
}


def get_model(name: str, reasoning: str = "default") -> ModelConfig:
    """Look up a model config by name.  Raises KeyError if unknown."""
    key = name.lower().strip()
    if key not in MODELS:
        available = ", ".join(sorted(MODELS))
        raise KeyError(f"Unknown model {name!r}. Available: {available}")
    cfg = MODELS[key]
    if reasoning != "default" and reasoning != cfg.reasoning:
        # Copy with the reasoning override. Earlier versions of this
        # branch passed positional args ending in `reasoning`, which
        # silently landed in `input_cost_per_m` (positional slot 4 in
        # the dataclass) and zeroed every cost-based metric for any
        # eval that overrode reasoning. Use keyword args here so a
        # future field addition can't reintroduce the same bug.
        return ModelConfig(
            name=cfg.name,
            provider=cfg.provider,
            backend=cfg.backend,
            backend_args=cfg.backend_args,
            input_cost_per_m=cfg.input_cost_per_m,
            output_cost_per_m=cfg.output_cost_per_m,
            reasoning=reasoning,
        )
    return cfg


# ---------------------------------------------------------------------------
# Condition specification
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ConditionSpec:
    """Configuration for one eval condition."""

    name: str
    cto_override: str | None  # "forceOff" or None (default CTO)
    uses_aethyme: bool
    repo_selector: str  # "control" | "aethyme"
    prompt_variant: str  # "baseline" | "leverage" | "task-conditioned"


CONDITIONS: tuple[ConditionSpec, ...] = (
    ConditionSpec("control-cto-off", "forceOff", False, "control", "baseline"),
    ConditionSpec("control-cto-on", None, False, "control", "baseline"),
    ConditionSpec("explore", None, True, "aethyme", "baseline"),
    ConditionSpec("leverage", None, True, "aethyme", "leverage"),
    ConditionSpec("task-conditioned", None, True, "aethyme", "task-conditioned"),
    # Trust-calibration condition (added 2026-05-09). Same skill + nav-context
    # *file* as leverage, but the file is a plausibly-wrong nav-context
    # generated against a sibling task in the same module. Isolates loading
    # cost (what leverage pays for blob ingestion) from misdirection cost
    # (what an agent loses by trusting the wrong content).
    #
    # Opt-in per eval type: only runs when the eval declares an
    # `alternative_task` in `_EVAL_TYPE_DEFAULTS`. See
    # `active_conditions_for()` below for the gating logic.
    ConditionSpec("negative-context", None, True, "aethyme", "negative-context"),
)


def active_conditions_for(eval_type: str) -> tuple[ConditionSpec, ...]:
    """Return the conditions that should run for this eval type.

    The 6th condition (`negative-context`) is opt-in — only runs when
    the eval declares an `alternative_task` in `_EVAL_TYPE_DEFAULTS`.
    Eval types without it (bug-fix-1, dead-code, impact-analysis,
    etc.) get the original 5 conditions.

    Why opt-in rather than always-on: the negative-context condition
    needs a *plausibly-wrong* nav-context blob to load, which the
    `bug_fix.py` flow generates via `generate_plausible_error_context`.
    The diagnostic eval types (driven by `prompts.py`) don't have a
    nav-context file in their flow — they're text-only prompts. Until
    the diagnostic flow grows nav-context support, those evals run
    with 5 conditions.

    Adding negative-context to a new eval type: declare
    `alternative_task` in its `_EVAL_TYPE_DEFAULTS` entry AND ensure
    its prepare/build-inputs phase generates the wrong nav-context
    artifact. The first condition is checked here; the second is on
    the contributor.
    """
    defaults = _EVAL_TYPE_DEFAULTS.get(eval_type, {})
    has_alternative = bool(defaults.get("alternative_task"))
    if has_alternative:
        return CONDITIONS
    return tuple(c for c in CONDITIONS if c.name != "negative-context")


# ---------------------------------------------------------------------------
# Eval-type defaults
# ---------------------------------------------------------------------------

# Per-eval-type defaults.
#
# Each entry declares:
# - `task`: prompt fed to the agent (the *what*).
# - `objective`: human-facing comparison statement — what we're using
#   the eval to compare across conditions, and which dimensions are
#   gates vs comparison axes. Rendered into the report header so the
#   reader doesn't have to reverse-engineer it from the table.
# - `constraints`: gates the answer must satisfy to count. Distinct
#   from rubric weights (those live in scoring); these are pre-rubric
#   admissibility checks like "valid JSON" or "didn't modify the
#   control repo." Rendered into the report header alongside the
#   objective.
# - `prepare_function`, `score_function`, `report_function`,
#   `target_restriction`: orchestration plumbing (unchanged).
#
# `objective` and `constraints` are NEVER inserted into agent
# prompts — they're for the human reader. Adding them to prompts
# would constitute eval-tuning by handing the agent the rubric.
_EVAL_TYPE_DEFAULTS: dict[str, dict[str, Any]] = {
    "bug-fix": {
        "task": (
            "Fix failing test: manage permission does not imply share "
            "in ability-implications.test.ts"
        ),
        # Sibling task used by the negative-context condition's
        # plausibly-wrong nav-context generator. Same package family
        # (auth) and conceptually adjacent (user-lifecycle vs permission
        # implications) — should land in the 30%-50% identifier-overlap
        # band that yields a "moderate mismatch difficulty" test.
        # The candidate must satisfy the 5-property plausibility rule
        # in `bug_fix.generate_plausible_error_context` at runtime; if
        # it doesn't, either widen the alternative or pick a closer one.
        "alternative_task": (
            "Fix bug: deleted users still appear in shared resource "
            "lists in the auth package"
        ),
        "objective": (
            "Compare cost across conditions to fix the failing "
            "ability-implications test (manage → share). Quality is a "
            "gate (test must pass post-fix); efficiency is the "
            "comparison axis."
        ),
        "constraints": (
            "The failing test must transition from FAIL to PASS, with "
            "previously-passing tests staying green.",
            "Output JSON must be saved to the agent-specified path.",
            "Fix may modify repository code (this eval applies a fix).",
        ),
        "prepare_function": "src.eval.bug_fix.prepare_bug_fix_benchmark",
        "score_function": "src.eval.bug_fix.assemble_bug_fix_result",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "bug-fix-1": {
        "task": (
            "Bug report (T419918): Viewing a diff/revision on a watchlisted "
            "page marks all revisions as 'seen' instead of only the one viewed. "
            "Identify which files need editing and explain how you would fix "
            "this bug. Do NOT apply the fix and do NOT write any files in the "
            "repository — produce your analysis as JSON in your final response "
            "only."
        ),
        "objective": (
            "Compare cost across conditions to localize T419918 "
            "(watchlist seen-marking) without applying a fix. "
            "Quality is a gate (the implementation file must be "
            "named); efficiency is the comparison axis."
        ),
        "constraints": (
            "Output must be valid JSON matching the documented schema.",
            "Output must be saved to the agent-specified path; "
            "missing or empty output scores 0.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.mediawiki_bug_fix_1_reference",
        "score_function": "src.eval.scoring.score_mediawiki_bug_fix_1",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
    "impact-analysis": {
        "task": (
            "WikiPage::doViewUpdates() in includes/Page/WikiPage.php is being refactored "
            "to accept different parameters. List every file that calls this method and "
            "would need updating.\n\n"
            "For each call site, provide: file path, line number, and the exact code at that line.\n\n"
            "Return a structured analysis with all call sites found."
        ),
        "objective": (
            "Compare cost across conditions to enumerate all callers "
            "of `WikiPage::doViewUpdates`. Recall is the gate (missed "
            "call sites = wrong answer); efficiency is the comparison."
        ),
        "constraints": (
            "Output must be valid JSON matching the documented schema.",
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.mediawiki_impact_analysis_reference",
        "score_function": "src.eval.scoring.score_mediawiki_bug_fix_1",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
    "feature-localization": {
        "task": (
            "When a user clicks 'Watch' on a wiki page, what code runs? "
            "Trace the full execution chain from the HTTP request handler to the "
            "database write.\n\n"
            "List each class and method in the chain, in execution order. "
            "For each step, provide: file path, method name, and a one-line "
            "description of what it does.\n\n"
            "Return a structured analysis with the complete chain."
        ),
        "objective": (
            "Compare cost across conditions to reconstruct the "
            "execution chain for the 'Watch' click path. Chain "
            "ordering and per-step file resolution are gates; "
            "efficiency is the comparison."
        ),
        "constraints": (
            "Output must be valid JSON matching the documented schema.",
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.mediawiki_feature_localization_reference",
        "score_function": "src.eval.scoring.score_mediawiki_bug_fix_1",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
    "config-audit": {
        "task": (
            "MediaWiki has rate limiting for API requests. Find:\n"
            "(a) The configuration variable that controls rate limits\n"
            "(b) Where the default value is defined (file and line)\n"
            "(c) The class that enforces rate limiting at runtime\n"
            "(d) How a site admin disables rate limiting for a specific action\n\n"
            "Return a structured analysis with all four answers."
        ),
        "objective": (
            "Compare cost across conditions to locate the four parts "
            "of MediaWiki's rate-limit configuration: variable name, "
            "default site, enforcement class, override mechanism. All "
            "four are gates; efficiency is the comparison."
        ),
        "constraints": (
            "Output must include all four fields "
            "(config_variable, default_definition, enforcement_class, "
            "disable_mechanism).",
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.mediawiki_config_audit_reference",
        "score_function": "src.eval.scoring.score_mediawiki_bug_fix_1",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
    "explain-repo": {
        "task": "Explain this repo",
        "objective": (
            "Compare cost across conditions to produce a high-level "
            "repo summary. Primary-language identification is a gate; "
            "coverage of top entry points contributes to quality."
        ),
        "constraints": (
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.explain_repo.run_explain_repo_evaluation",
        "score_function": "src.eval.scoring.score_explain_repo_output",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "navigation-ctf": {
        "task": "Find the manifest that manages the main code entrypoint",
        "objective": (
            "Compare cost across conditions to locate the named "
            "manifest file. The path must be returned exactly; "
            "partial matches do not count."
        ),
        "constraints": (
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.navigation_ctf.run_navigation_ctf_evaluation",
        "score_function": "src.eval.scoring.score_navigation_ctf_output",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "dead-code": {
        "task": "Target-specific dead-code evaluation",
        "objective": (
            "Compare cost across conditions to enumerate unreachable "
            "functions in the target scope. Precision is the gate "
            "(false positives — live code listed as dead — are worse "
            "than false negatives); efficiency is the comparison."
        ),
        "constraints": (
            "Output must be valid JSON matching the documented schema.",
            "Output must be saved to the agent-specified path.",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.dead_code_reference_for_target",
        "score_function": "src.eval.scoring.score_dead_code",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "migration": {
        "task": (
            "List every file referencing WatchedItemStore that would need "
            "updating if the class were renamed to WatchlistNotificationStore."
        ),
        "objective": (
            "Compare cost across conditions to enumerate all "
            "references to `WatchedItemStore` that need updating on "
            "rename. Recall is the gate (missed references = broken "
            "rename); efficiency is the comparison."
        ),
        "constraints": (
            "Output must be valid JSON matching the documented schema.",
            "Vendor files must be excluded.",
            "Each reference must be labeled with kind (instantiation, "
            "type-hint, use-import, service-locator, docblock).",
            "Repository files must not be modified.",
        ),
        "prepare_function": "src.eval.schemas.mediawiki_migration_reference",
        "score_function": "src.eval.scoring.score_mediawiki_migration",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
}

_AETHYME_PKG = str(PROJECT_ROOT)
_AETHYME_VENV_PYTHON = str(PROJECT_ROOT / ".venv" / "bin" / "python")


def get_eval_type_contract(eval_type: str) -> tuple[str, list[str]]:
    """Return (objective, constraints) for an eval type.

    Single source of truth for the comparison contract — what we are
    using the eval to compare across conditions, and which gates the
    answer must pass to count. The report layer reads this at render
    time so the table is annotated with the same statement that the
    eval was designed against.

    Returns ``("", [])`` for unknown eval types so the render layer
    can no-op gracefully on legacy results that predate the contract.
    """
    defaults = _EVAL_TYPE_DEFAULTS.get(eval_type, {})
    return (
        defaults.get("objective", ""),
        list(defaults.get("constraints", ())),
    )


# ---------------------------------------------------------------------------
# Core plan generator
# ---------------------------------------------------------------------------


def generate_run_plan(
    *,
    eval_type: str,
    target: str,
    model: str,
    scenario: str | None = None,
    reasoning: str = "default",
    dest_dir: str | None = None,
    tool: str | None = None,
) -> dict[str, Any]:
    """Generate a complete eval run plan.

    PURE FUNCTION — no side effects, no Chau7 calls, no file writes.
    Returns a structured dict that the orchestrating agent (Claude)
    runs mechanically via Chau7 MCP.

    ``tool`` selects which tool adapter populates the explore / leverage /
    task-conditioned conditions. Defaults to the target's ``default_tool``,
    which itself defaults to the framework's self-tool (see
    :mod:`src.eval._self` — ``AETHYMEBENCH_SELF_TOOL`` env var, ``"aethyme"``
    if unset). Pass ``tool="graphify"`` to swap in a competitor manifest.
    """
    if eval_type not in _EVAL_TYPE_DEFAULTS:
        raise ValueError(
            f"Unknown eval_type {eval_type!r}. "
            f"Available: {', '.join(sorted(_EVAL_TYPE_DEFAULTS))}"
        )

    eval_target = get_target(target)
    model_config = get_model(model, reasoning)
    tool_name = tool or eval_target.default_tool

    # Resolve the adapter early so manifest validation errors surface
    # before any phase work happens. Imported here (not module-level) to
    # avoid an import cycle with src.eval.tools, which depends on
    # src.eval.targets via the registry's default lookup.
    from .tools import get_adapter
    tool_adapter = get_adapter(tool_name)

    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S")

    # Run-dir naming contract:
    #
    #   {timestamp}-{target}-[{scenario}-]{eval-type}-{model}[-{reasoning}][-{tool}]
    #
    # Each segment beyond timestamp+target+eval-type is included only when it
    # adds discrimination value:
    #   - `scenario`: present for bug-fix's cross-package variant; preserves
    #     the prior naming convention exactly when scenario is set.
    #   - `model`: ALWAYS present so eval-runs/ is greppable by model.
    #   - `reasoning`: omitted when default; appended otherwise.
    #   - `tool`: omitted when it's the framework's self-tool (see
    #     :mod:`src.eval._self` — ``AETHYMEBENCH_SELF_TOOL`` env var, default
    #     ``"aethyme"``). Bare slugs are reserved for runs of the framework's
    #     own subject; competitor manifests always get an explicit suffix.
    #
    # Examples this generates:
    #   20260515T055924-mediawiki-dead-code-haiku
    #   20260515T155500-mediawiki-dead-code-haiku-low
    #   20260515T155500-grc-bug-fix-haiku-graphify
    #   20260515T155500-grc-cross-package-bug-fix-gpt-5.4-high-graphify
    slug_parts: list[str] = [eval_target.name]
    if scenario:
        slug_parts.append(scenario)
    slug_parts.append(eval_type)
    slug_parts.append(model_config.name)
    if reasoning != "default":
        slug_parts.append(reasoning)
    # Omit tool suffix for the framework's self-tool (see :mod:`src.eval._self`):
    # a fork that renames its subject via AETHYMEBENCH_SELF_TOOL gets bare slugs
    # for *its* subject, and `-graphify` / `-aethyme` suffixes only when running
    # a non-self-tool variant.
    if not is_self_tool(tool_name):
        slug_parts.append(tool_name)
    slug = "-".join(slug_parts)

    # Default dest_dir for bug-fix clones
    if eval_type == "bug-fix" and dest_dir is None:
        dest_dir = str(
            Path("/tmp").resolve() / f"benchmark-{eval_target.name}-{timestamp}"
        )

    run_dir_name = f"{timestamp}-{slug}"
    paths = _build_paths(eval_type, dest_dir, run_dir_name)

    eval_defaults = _EVAL_TYPE_DEFAULTS[eval_type]
    meta = {
        "eval_type": eval_type,
        "target": eval_target.name,
        "target_display": eval_target.display_name,
        "scenario": scenario,
        "model": model_config.to_dict(),
        "aethyme_commit": get_aethyme_commit(),
        "aethyme_root": _AETHYME_PKG,
        "tool": tool_name,
        "tool_display": tool_adapter.display_name,
        "tool_manifest_notes": tool_adapter.manifest.condition_mapping_note
            if hasattr(tool_adapter, "manifest") else "",
        "timestamp": datetime.now(UTC).isoformat(),
        "conditions": [c.name for c in active_conditions_for(eval_type)],
        # Comparison contract — propagated into the report header so a
        # reader looking at the table knows what is being compared and
        # what counts as admissible. NOT inserted into agent prompts.
        "objective": eval_defaults.get("objective", ""),
        "constraints": list(eval_defaults.get("constraints", ())),
    }

    phases = [
        _build_validate_phase(eval_target),
        _build_prepare_phase(eval_type, eval_target, scenario, dest_dir, paths, tool_name),
        _build_warm_phase(eval_target, tool_adapter),
        _build_launch_phase(eval_type, eval_target, model_config, dest_dir, paths),
        _build_monitor_phase(),
        _build_collect_phase(eval_type, model_config, paths),
        _build_score_phase(eval_type, scenario, model_config),
        _build_report_phase(eval_type, paths),
        _build_cleanup_phase(),
    ]

    return {"meta": meta, "paths": paths, "phases": phases}


# ---------------------------------------------------------------------------
# Path computation
# ---------------------------------------------------------------------------


def _build_paths(
    eval_type: str,
    dest_dir: str | None,
    run_dir_name: str,
) -> dict[str, Any]:
    """Compute all artifact paths up front."""
    tmp = str(Path("/tmp").resolve())
    active = active_conditions_for(eval_type)

    prompt_files = {
        c.name: f"{tmp}/aethyme-eval-{c.name}-prompt.txt" for c in active
    }
    result_files = {
        c.name: f"{tmp}/aethyme-eval-{c.name}-result.json" for c in active
    }

    paths: dict[str, Any] = {
        "aethyme_root": _AETHYME_PKG,
        "venv_python": _AETHYME_VENV_PYTHON,
        "run_dir": f"eval-runs/{run_dir_name}",
        "prompt_files": prompt_files,
        "result_files": result_files,
        "schema_file": f"{tmp}/aethyme-eval-output-schema.json",
        "nav_context_file": f"{tmp}/aethyme-eval-navigation-context.json",
    }

    if eval_type == "bug-fix" and dest_dir:
        paths["dest_dir"] = dest_dir
        paths["condition_repos"] = {
            c.name: f"{dest_dir}/{c.name}" for c in active
        }

    return paths


# ---------------------------------------------------------------------------
# Phase builders
# ---------------------------------------------------------------------------


def _build_validate_phase(target: EvalTarget) -> dict[str, Any]:
    checks = [
        {
            "check": "control_repo_exists",
            "path": str(target.control_path),
            "expected": True,
        },
        {
            "check": "control_is_git_repo",
            "path": str(target.control_path / ".git"),
            "expected": True,
        },
        {
            "check": "control_no_skill_contamination",
            "path": str(target.control_path / ".codex" / "skills"),
            "expected": False,
        },
        {
            "check": "aethyme_repo_exists",
            "path": str(target.aethyme_path),
            "expected": True,
        },
        {
            "check": "aethyme_is_git_repo",
            "path": str(target.aethyme_path / ".git"),
            "expected": True,
        },
        {
            "check": "aethyme_has_skill",
            "path": str(target.aethyme_path / ".codex" / "skills"),
            "expected": True,
        },
        {
            "check": "engine_binary_exists",
            "path": str(
                PROJECT_ROOT / "rust" / "target" / "release" / "aethyme-engine-cli"
            ),
            "expected": True,
        },
        {
            "check": "global_skill_absent",
            "path": str(
                Path.home() / ".codex" / "skills" / "aethyme-navigation"
            ),
            "expected": False,
        },
    ]
    return {
        "name": "prepare",
        "description": "Check repository readiness and persist a preparation snapshot",
        "checks": checks,
    }


def _build_warm_phase(target: EvalTarget, tool_adapter: Any = None) -> dict[str, Any]:
    """Pre-warm the configured tool against the target's repo.

    Cold start on MediaWiki is ~108s map_build + ~16s signals (measured
    2026-05-07). If the tool isn't pre-warmed, the leverage condition's
    first call eats that cost serially. Warming before `launch` makes
    per-condition timing comparable.

    For small repos (e.g. Mockup, ~7K files) cold start is ~6s, but the
    phase is cheap to run and keeps the contract uniform across targets.

    Reads ``[warm].command`` from the tool's manifest when an adapter is
    supplied; falls back to the legacy hardcoded Aethyme daemon path when
    ``tool_adapter`` is None (preserves existing plan output for callers
    that haven't migrated yet).
    """
    # Adapter-driven path (post-migration).
    if tool_adapter is not None and getattr(tool_adapter, "manifest", None) is not None:
        manifest = tool_adapter.manifest
        if manifest.warm_command is not None:
            tool_root = str(tool_adapter.tool_root)
            target_repo = str(target.aethyme_path)
            cli_cmd = tool_adapter.render_command(
                manifest.warm_command,
                target_repo=target.aethyme_path,
            )
            phase: dict[str, Any] = {
                "name": "warm",
                "description": (
                    f"Pre-warm {manifest.display_name} against "
                    f"{target.display_name} so leverage condition doesn't eat "
                    f"cold-start cost"
                ),
                "tool": manifest.name,
                "tool_root": tool_root,
                "target_repo": target_repo,
                "cli_cmd": cli_cmd,
                "wait_for": "listening on" if manifest.name == "aethyme" else None,
                "max_wait_seconds": 240,
                "instructions": (
                    f"Run the cli_cmd to warm {manifest.display_name}'s "
                    f"caches/indexes against {target.display_name}. The "
                    f"command should be idempotent."
                ),
            }
            # Legacy field projection for tool=aethyme so tests written
            # against the pre-manifest warm phase continue to pass. New
            # tests should consult cli_cmd directly; these fields are
            # transitional and removable once test_eval_warm_phase.py is
            # rewritten in a follow-up to inspect cli_cmd content.
            if manifest.name == "aethyme":
                engine_bin = str(
                    PROJECT_ROOT / "rust" / "target" / "release" / "aethyme-engine-cli"
                )
                phase["engine_bin"] = engine_bin
                phase["aethyme_repo"] = target_repo
                phase["log_path"] = str(target.aethyme_path / ".aethyme" / "engine-daemon.log")
            return phase
        # Tool has no [warm] block — emit a no-op phase rather than dropping
        # it entirely, so the plan's phase count stays stable across tools.
        return {
            "name": "warm",
            "description": f"No warm step for {manifest.display_name}",
            "tool": manifest.name,
            "cli_cmd": ":",   # shell no-op
            "wait_for": None,
            "max_wait_seconds": 1,
            "instructions": "This tool's manifest declares no [warm] block; skip.",
        }

    # Legacy path — preserved for callers that don't pass an adapter.
    # Behavior identical to the pre-manifest plan output.
    engine_bin = str(
        PROJECT_ROOT / "rust" / "target" / "release" / "aethyme-engine-cli"
    )
    aethyme_repo = str(target.aethyme_path)
    log_path = str(target.aethyme_path / ".aethyme" / "engine-daemon.log")

    return {
        "name": "warm",
        "description": (
            f"Pre-warm engine daemon for {target.display_name} so leverage "
            f"condition doesn't eat cold-start cost (≤108s on MediaWiki, "
            f"≤6s on Mockup)"
        ),
        "engine_bin": engine_bin,
        "aethyme_repo": aethyme_repo,
        "log_path": log_path,
        "cli_cmd": (
            f"\"{engine_bin}\" daemon status --repo \"{aethyme_repo}\" "
            f"|| (\"{engine_bin}\" daemon start --repo \"{aethyme_repo}\" "
            f"&& while ! tail -1 \"{log_path}\" 2>/dev/null | grep -q "
            f"'listening on'; do sleep 5; done)"
        ),
        "wait_for": "listening on",
        "max_wait_seconds": 240,
        "instructions": (
            "If the daemon is already running for the target Aethyme repo, "
            "skip. Otherwise: spawn it with `aethyme-engine-cli daemon start "
            "--repo <path>` and poll the log file at log_path until the line "
            "matching wait_for appears. Bail with a warning after "
            "max_wait_seconds — the eval can still run, but leverage will pay "
            "the cold-start cost."
        ),
        "skippable_when": (
            "Caller passes --no-warm OR the daemon socket already exists. "
            "Always emit the phase; runners decide whether to execute it."
        ),
    }


def _build_prepare_phase(
    eval_type: str,
    target: EvalTarget,
    scenario: str | None,
    dest_dir: str | None,
    paths: dict[str, Any],
    tool_name: str | None = None,
) -> dict[str, Any]:
    """Build the artifact generation phase.

    When ``tool_name`` is supplied (e.g. "aethyme", "graphify"), it is
    injected into the emitted cli_cmd as a ``--tool`` flag. The
    receiving CLI subcommand loads the matching adapter and routes
    leverage-data generation through it. ``tool_name=None`` keeps the
    legacy direct-Python path active, preserving byte-identical output
    for callers that don't yet plumb --tool.

    Tool-using adapter integration (subprocess-mediated leverage /
    task-conditioned data) is currently supported for ``bug-fix``,
    ``explain-repo``, and ``navigation-ctf``. Diagnostic eval types
    (``bug-fix-1``, ``dead-code``, etc.) use the ``prompts_writer``
    flow, which since Stage B.2.1 *does* honor ``--tool`` — but only
    to bind the manifest's ``[prompts]`` hint text into the prompt
    addendum, not to invoke any tool subprocess. Either way the flag
    is methodologically meaningful for every eval type.
    """
    venv = paths["venv_python"]
    pkg = paths["aethyme_root"]
    tool_flag = f" --tool {tool_name}" if tool_name else ""

    if eval_type == "bug-fix":
        scenario_flag = f" --scenario {scenario}" if scenario else ""
        # Propagate the alternative task for the negative-context condition.
        # Single-quoted in shell to survive embedded apostrophes / commas;
        # task strings shouldn't contain single quotes (we control them in
        # `_EVAL_TYPE_DEFAULTS`), but if a future contributor adds one,
        # the CLI will surface a clear shell error rather than silently
        # truncate.
        alt_task = _EVAL_TYPE_DEFAULTS[eval_type].get("alternative_task")
        alt_flag = f" --alternative-task '{alt_task}'" if alt_task else ""
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval bug-fix prepare"
            f" --source '{target.control_path}'"
            f" --dest '{dest_dir}'"
            f"{scenario_flag}"
            f"{alt_flag}"
            f"{tool_flag}"
            f" --json-output"
        )
        description = (
            f"Clone {len(CONDITIONS)} repos from {target.display_name} "
            f"control, plant bug, generate all artifacts (incl. negative "
            f"nav-context)"
        )
    elif eval_type in ("bug-fix-1", "dead-code", "impact-analysis",
                       "feature-localization", "config-audit", "migration"):
        # Read-only diagnostic eval — no cloning. The orchestrator writes
        # all 5 condition prompts via `src.eval.prompts_writer`, which
        # delegates to the unit-tested `src.eval.prompts.build_prompts`.
        # Replaces the old inline `python -c "..."` template (fragile —
        # em-dashes and quoted JSON inside `task` strings repeatedly
        # broke shell quoting on bug-fix-1).
        prompt_args = " ".join(
            f"--prompt-out {cond.name}={paths['prompt_files'][cond.name]}"
            for cond in active_conditions_for(eval_type)
        )
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.eval.prompts_writer"
            f" --eval-type {eval_type}"
            f" --target {target.name}"
            f"{tool_flag}"
            f" --schema-out {paths['schema_file']}"
            f" {prompt_args}"
        )
        description = f"Generate {eval_type} prompts + schema for {target.display_name}"
    elif eval_type == "explain-repo":
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval explain-repo"
            f" --repo '{target.aethyme_path}'"
            f"{tool_flag}"
            f" --json-output"
        )
        description = f"Generate explain-repo artifacts for {target.display_name}"
    elif eval_type == "navigation-ctf":
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval navigation-ctf"
            f" --repo '{target.aethyme_path}'"
            f"{tool_flag}"
            f" --json-output"
        )
        description = f"Generate navigation-ctf artifacts for {target.display_name}"
    else:
        raise ValueError(f"Unknown eval_type: {eval_type}")

    return {
        "name": "build-inputs",
        "description": description,
        "cli_cmd": cli_cmd,
        "writes_to": list(paths["prompt_files"].values()) + [
            paths["schema_file"],
            paths["nav_context_file"],
        ],
    }


def _build_launch_phase(
    eval_type: str,
    target: EvalTarget,
    model_config: ModelConfig,
    dest_dir: str | None,
    paths: dict[str, Any],
) -> dict[str, Any]:
    """Build per-condition launch instructions."""
    conditions_launch: list[dict[str, Any]] = []

    for cond in active_conditions_for(eval_type):
        directory = _resolve_condition_dir(eval_type, cond, target, dest_dir)
        prompt_file = paths["prompt_files"][cond.name]
        result_file = paths["result_files"][cond.name]
        schema_file = paths["schema_file"]

        entry: dict[str, Any] = {
            "name": cond.name,
            "directory": directory,
            "prompt_file": prompt_file,
            "result_file": result_file,
            "cto_override": cond.cto_override,
        }

        if model_config.backend == "claude":
            args = list(model_config.backend_args)
            entry["launch_method"] = "tab_create_then_exec_submit"
            entry["tab_create_params"] = {
                "directory": directory,
            }
            entry["tab_exec_args"] = [
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                model_config.name,
                *args,
            ]
            entry["prompt_source"] = "file"
            entry["note"] = (
                "Create a Chau7 tab, launch Claude via tab_exec, then send the "
                "prompt with tab_send_input and tab_submit_prompt. "
                "Launch with an explicit --session-id when deterministic "
                "collection is required."
            )
        elif model_config.backend == "codex":
            codex_args = ["codex", "exec"] + list(model_config.backend_args) + [
                "--output-schema", schema_file,
                "--output-last-message", result_file,
            ]
            if model_config.reasoning != "default":
                codex_args += ["-c", f'reasoning_effort="{model_config.reasoning}"']

            entry["launch_method"] = "tab_create_then_exec"
            entry["tab_create_params"] = {"directory": directory}
            entry["tab_exec_args"] = codex_args
            entry["note"] = (
                "Create tab with tab_create(directory=...), "
                "then run codex via tab_exec. "
                "Prompt is read from prompt_file and appended to command."
            )

        conditions_launch.append(entry)

    return {
        "name": "launch",
        "description": (
            f"Launch {len(conditions_launch)} agent sessions "
            f"({model_config.backend}/{model_config.name})"
        ),
        "backend": model_config.backend,
        "model": model_config.name,
        "conditions": conditions_launch,
    }


def _build_monitor_phase() -> dict[str, Any]:
    return {
        "name": "monitor",
        "description": "Poll result files until all sessions complete",
        "poll_interval_seconds": 15,
        "timeout_seconds": 1800,
        "instructions": (
            "Use result-file polling as the completion signal for every "
            "condition. Poll tab_status(tab_id) only as diagnostic shell/tab "
            "state; do not treat is_at_prompt or ai_session_id as authoritative "
            "completion or attribution evidence."
        ),
    }


def _build_collect_phase(
    eval_type: str,
    model_config: ModelConfig,
    paths: dict[str, Any],
) -> dict[str, Any]:
    per_condition: list[dict[str, Any]] = []

    for cond in active_conditions_for(eval_type):
        result_file = paths["result_files"][cond.name]

        collect_entry: dict[str, Any] = {
            "name": cond.name,
            "result_file": result_file,
            "chau7_calls": [
                {"tool": "tab_output", "params": {"lines": 5000}},
                {
                    "tool": (
                        "session_jsonl_and_tab_status"
                        if model_config.backend == "claude"
                        else "tab_status"
                    ),
                },
            ],
            "telemetry_calls": [
                {"tool": "run_get"},
                {"tool": "run_transcript"},
                {"tool": "run_tool_calls"},
            ],
        }

        if model_config.backend == "claude":
            collect_entry["result_extraction"] = (
                "Extract output from the file-first capture path or from the "
                "agent's final message in the transcript or session JSONL"
            )
        else:
            collect_entry["result_extraction"] = (
                f"Read structured output from {result_file}"
            )

        per_condition.append(collect_entry)

    return {
        "name": "collect",
        "description": "Gather results, telemetry, and outputs per condition",
        "per_condition": per_condition,
        "storage": [
            "store_condition_raw(run_dir, condition, stdout, structured_output)",
            "store_condition_chau7(run_dir, condition, run_id, transcript, tool_calls, attribution_confidence, completion_provenance)",
            "write conditions/<condition>/attribution-confidence.json",
            "write conditions/<condition>/completion-provenance.json",
        ],
        "attribution_contract": (
            "Record reported Chau7 ai_session_id, content-matched JSONL path, "
            "matched output-file marker, and mismatch boolean. Prefer "
            "content-matched JSONL over tab_status.ai_session_id for collection."
        ),
        "completion_contract": (
            "Record result-file-seen timestamp, transcript-matched timestamp, "
            "and final collection source. Result-file polling is the primary "
            "completion signal."
        ),
    }


def _build_score_phase(
    eval_type: str,
    scenario: str | None,
    model_config: ModelConfig,
) -> dict[str, Any]:
    defaults = _EVAL_TYPE_DEFAULTS[eval_type]

    return {
        "name": "score",
        "description": f"Score and assemble result via {defaults['score_function']}",
        "eval_type": eval_type,
        "scenario": scenario,
        "score_function": defaults["score_function"],
        "model_metadata": model_config.to_dict(),
        "instructions": (
            "For bug-fix: call assemble_bug_fix_result(conditions={...}, "
            f"task=..., repo_path=..., scenario={scenario!r}, model=...). "
            "For other types: score each condition individually, "
            "then build result dict with EvaluationSide per condition."
        ),
    }


def _build_report_phase(
    eval_type: str,
    paths: dict[str, Any],
) -> dict[str, Any]:
    return {
        "name": "report",
        "description": "Generate report via code pipeline (never hand-write)",
        "report_function": _EVAL_TYPE_DEFAULTS[eval_type]["report_function"],
        "run_dir": paths["run_dir"],
        "instructions": (
            "Call finalize_eval_run(run_dir, result, repo_path=..., eval_type=...) "
            "then print_scorecard(result) to display in chat. "
            "NEVER hand-write markdown reports."
        ),
    }


def _build_cleanup_phase() -> dict[str, Any]:
    return {
        "name": "cleanup",
        "description": "Close all agent sessions and tabs",
        "instructions": (
            "Close each tab with tab_close(tab_id, force=True). "
            "For Claude-backed runs, stop the app session first if the backend "
            "exposes a direct stop call, but treat tab closure as mandatory. "
            "CRITICAL: Always close tabs — Chau7 has a limited tab pool."
        ),
    }


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _resolve_condition_dir(
    eval_type: str,
    condition: ConditionSpec,
    target: EvalTarget,
    dest_dir: str | None,
) -> str:
    """Return the working directory for a condition's agent session."""
    if eval_type == "bug-fix":
        if dest_dir is None:
            raise ValueError("dest_dir required for bug-fix evals")
        return str(Path(dest_dir) / condition.name)

    # Read-only evals: control → control repo, aethyme → aethyme repo
    if condition.repo_selector == "control":
        return str(target.control_path.resolve())
    return str(target.aethyme_path.resolve())
