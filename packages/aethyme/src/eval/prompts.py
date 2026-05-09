"""Per-condition prompt construction for read-only diagnostic eval types.

For each `eval_type` whose `build-inputs` phase is "write 5 prompts to
/tmp/", this module exposes `build_<type>_prompts(target) -> dict[cond, str]`
returning the full prompt text for every condition. The orchestrator
calls these via a small Python invocation in the build-inputs phase
instead of inlining the templates as `python -c "..."` strings (which
have shell-quoting bugs — we hit them on bug-fix-1).

Five conditions per eval type, all in fixed shape:

    control-cto-off    → repo = control,    no Aethyme guidance
    control-cto-on     → repo = control,    no Aethyme guidance (default CTO)
    explore            → repo = Aethyme,    no Aethyme guidance ("discoverability test")
    leverage           → repo = Aethyme,    explicit Aethyme tool + intent hint
    task-conditioned   → repo = Aethyme,    "use task-conditioned context artifacts"

The control vs Aethyme distinction is which repo the prompt names AND
where the agent writes its output JSON. Scoring reads back from
`<repo>/.aethyme-eval-output-<condition>.json`.

Adding a new eval type? Define `build_<type>_prompts(target)` here and
add a `("<type>", build_<type>_prompts)` row to `BUILDERS` below.
"""

from __future__ import annotations

from collections.abc import Callable

from .targets import EvalTarget

CONDITION_NAMES: tuple[str, ...] = (
    "control-cto-off",
    "control-cto-on",
    "explore",
    "leverage",
    "task-conditioned",
)


# The leverage condition's prompt addition: a *minimal* pointer at the
# tool, not a step-by-step guide. The whole point of the leverage-vs-
# explore comparison is to measure the cost of "agent told the tool
# exists" vs "agent has skill loaded but no instruction."
#
# An earlier version of this hint named the canonical intent
# (`usage_boundary_query`, `behavior_localization_query`) and listed
# the response fields to read. That biased the discoverability gap
# upward — we were measuring "great prompt engineering" rather than
# "tool was pointed at." The current shape just names the entry point;
# the agent must read SKILL.md (or experiment) to learn how to invoke
# it. That's the honest measurement.
_LEVERAGE_HINT = (
    "Aethyme is available in this repository. See "
    "`.codex/skills/aethyme/SKILL.md` for usage; the wrapper at "
    "`.codex/skills/aethyme/aethyme-explore` is the convenience "
    "entry point. Use it where it helps; verify its output before "
    "acting on it.\n\n"
)


# ---------------------------------------------------------------------------
# Shared shape: every diagnostic prompt has a preamble (output-file rule),
# the task itself, an optional condition-specific prefix (Aethyme guidance
# for leverage/task-conditioned), and a postamble (reminder to save JSON).
# ---------------------------------------------------------------------------


def _output_path(target: EvalTarget, condition: str) -> str:
    """Where the agent writes its JSON output for `condition`."""
    repo = target.control_path if condition.startswith("control") else target.aethyme_path
    return f"{repo}/.aethyme-eval-output-{condition}.json"


def _preamble(target: EvalTarget, condition: str, schema_shape: str) -> str:
    output_path = _output_path(target, condition)
    return (
        f"IMPORTANT: You MUST save exactly one JSON object to "
        f"`{output_path}` when done. Use the Write tool to create this "
        f"file. The file contents must be valid JSON with exactly this "
        f"shape:\n{schema_shape}\n\n"
    )


def _postamble(target: EvalTarget, condition: str) -> str:
    return f"\n\nRemember: save JSON only to `{_output_path(target, condition)}`."


def _build_per_condition(
    target: EvalTarget,
    task: str,
    schema_shape: str,
    leverage_hint: str = "",
    task_conditioned_hint: str = "",
) -> dict[str, str]:
    """Standard 5-condition prompt assembly.

    The `leverage_hint` is the Aethyme-specific guidance that appears
    only in the leverage condition. `task_conditioned_hint` is similar
    but for the task-conditioned condition; if empty, defaults to a
    generic "use task-conditioned context artifacts" line.
    """
    if not task_conditioned_hint:
        task_conditioned_hint = (
            "Use Aethyme tools and any task-conditioned context "
            "artifacts to navigate the repository graph, but do your "
            "own analysis.\n\n"
        )

    out: dict[str, str] = {}
    for cond in CONDITION_NAMES:
        prefix = ""
        if cond == "leverage" and leverage_hint:
            prefix = leverage_hint
        elif cond == "task-conditioned" and task_conditioned_hint:
            prefix = task_conditioned_hint
        out[cond] = (
            _preamble(target, cond, schema_shape)
            + task
            + "\n\n"
            + prefix
            + _postamble(target, cond)
        ).strip() + "\n"
    return out


# ---------------------------------------------------------------------------
# bug-fix-1 (T419918 watchlist diagnostic). Re-implementation of the
# inline orchestrator template; the cli.py shell-quoting bug is gone.
# ---------------------------------------------------------------------------


_BUG_FIX_1_TASK = (
    "Bug report (T419918): Viewing a diff/revision on a watchlisted "
    "page marks all revisions as 'seen' instead of only the one viewed. "
    "Identify which files need editing and explain how you would fix "
    "this bug. Do NOT apply the fix and do NOT write any files in the "
    "repository — produce your analysis as JSON in your final response "
    "only."
)

_BUG_FIX_1_SCHEMA_SHAPE = (
    "{\n"
    '  "files_to_edit": [\n'
    '    {"path": "relative/path.php", "what_to_change": "..."}\n'
    '  ],\n'
    '  "root_cause": "...",\n'
    '  "fix_plan": "...",\n'
    '  "testing": "..."\n'
    "}"
)


def build_bug_fix_1_prompts(target: EvalTarget) -> dict[str, str]:
    """T419918 watchlist diagnostic. MediaWiki-only."""
    return _build_per_condition(
        target=target,
        task=_BUG_FIX_1_TASK,
        schema_shape=_BUG_FIX_1_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# dead-code (Watchlist boundary). Mirrors today's manual template
# (2026-05-08 validation run) — that was used as the canonical reference.
# ---------------------------------------------------------------------------


_DEAD_CODE_TASK = (
    "Find all public methods in `includes/Watchlist/` that are never "
    "called from outside that directory.\n\n"
    "Scope:\n"
    "- Check every PHP file in `includes/Watchlist/` for public function definitions\n"
    "- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites\n"
    "- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search\n"
    "- Exclude constructors (`__construct`, `__destruct`)\n\n"
    "For each unused function, report:\n"
    "- The function name\n"
    "- The file it's defined in (relative path)\n"
    "- Why you believe it's unused (what you searched for and didn't find)\n\n"
    "Be thorough — check every public function, not just a sample. "
    "Missing a truly unused function or falsely flagging a used one "
    "both count against you."
)

_DEAD_CODE_SCHEMA_SHAPE = (
    "{\n"
    '  "unused_functions": [\n'
    "    {\n"
    '      "function_name": "name",\n'
    '      "defined_in": "relative/path.py",\n'
    '      "reason": "what you searched for and did not find"\n'
    "    }\n"
    "  ]\n"
    "}"
)


def build_dead_code_prompts(target: EvalTarget) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_DEAD_CODE_TASK,
        schema_shape=_DEAD_CODE_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# impact-analysis (refactor footprint). MediaWiki-only.
# ---------------------------------------------------------------------------


_IMPACT_ANALYSIS_TASK = (
    "WikiPage::doViewUpdates() in includes/Page/WikiPage.php is being "
    "refactored to accept different parameters. List every file that "
    "calls this method and would need updating.\n\n"
    "For each call site, provide: file path, line number, and the exact "
    "code at that line.\n\n"
    "Return a structured analysis with all call sites found. Do NOT "
    "modify any files in the repository."
)

_IMPACT_ANALYSIS_SCHEMA_SHAPE = (
    "{\n"
    '  "call_sites": [\n'
    '    {"path": "relative/path.php", "line": 123, "code": "..."}\n'
    "  ]\n"
    "}"
)


def build_impact_analysis_prompts(target: EvalTarget) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_IMPACT_ANALYSIS_TASK,
        schema_shape=_IMPACT_ANALYSIS_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# feature-localization (execution chain trace). MediaWiki-only.
# ---------------------------------------------------------------------------


_FEATURE_LOCALIZATION_TASK = (
    "When a user clicks 'Watch' on a wiki page, what code runs? Trace "
    "the full execution chain from the HTTP request handler to the "
    "database write.\n\n"
    "List each class and method in the chain, in execution order. For "
    "each step, provide: file path, method name, and a one-line "
    "description of what it does.\n\n"
    "Return a structured analysis with the complete chain. Do NOT "
    "modify any files in the repository."
)

_FEATURE_LOCALIZATION_SCHEMA_SHAPE = (
    "{\n"
    '  "execution_chain": [\n'
    "    {\n"
    '      "step": 1,\n'
    '      "path": "relative/path.php",\n'
    '      "method": "ClassName::method",\n'
    '      "description": "what it does"\n'
    "    }\n"
    "  ]\n"
    "}"
)


def build_feature_localization_prompts(target: EvalTarget) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_FEATURE_LOCALIZATION_TASK,
        schema_shape=_FEATURE_LOCALIZATION_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# config-audit (config + enforcement + override). MediaWiki-only.
# ---------------------------------------------------------------------------


_CONFIG_AUDIT_TASK = (
    "MediaWiki has rate limiting for API requests. Find:\n"
    "(a) The configuration variable that controls rate limits\n"
    "(b) Where the default value is defined (file and line)\n"
    "(c) The class that enforces rate limiting at runtime\n"
    "(d) How a site admin disables rate limiting for a specific action\n\n"
    "Return a structured analysis with all four answers. Do NOT modify "
    "any files in the repository."
)

_CONFIG_AUDIT_SCHEMA_SHAPE = (
    "{\n"
    '  "config_variable": "name",\n'
    '  "default_definition": {"path": "...", "line": 0},\n'
    '  "enforcement_class": {"path": "...", "name": "..."},\n'
    '  "disable_mechanism": "description of how to disable per-action"\n'
    "}"
)


def build_config_audit_prompts(target: EvalTarget) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_CONFIG_AUDIT_TASK,
        schema_shape=_CONFIG_AUDIT_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# migration (rename impact). MediaWiki-only.
# ---------------------------------------------------------------------------


_MIGRATION_TASK = (
    "List every file referencing `WatchedItemStore` that would need "
    "updating if the class were renamed to `WatchlistNotificationStore`.\n\n"
    "Include all references: class instantiation, type hints, imports, "
    "service-locator lookups, docblocks. Exclude vendor files.\n\n"
    "For each reference, provide: file path, line number, and the kind "
    "of reference (instantiation / type-hint / use-import / "
    "service-locator / docblock).\n\n"
    "Return a structured analysis with all references found. Do NOT "
    "modify any files in the repository."
)

_MIGRATION_SCHEMA_SHAPE = (
    "{\n"
    '  "references": [\n'
    "    {\n"
    '      "path": "relative/path.php",\n'
    '      "line": 123,\n'
    '      "kind": "instantiation | type-hint | use-import | service-locator | docblock"\n'
    "    }\n"
    "  ]\n"
    "}"
)


def build_migration_prompts(target: EvalTarget) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_MIGRATION_TASK,
        schema_shape=_MIGRATION_SCHEMA_SHAPE,
        leverage_hint=_LEVERAGE_HINT,
    )


# ---------------------------------------------------------------------------
# Registry: lookup table the orchestrator uses.
# ---------------------------------------------------------------------------


BUILDERS: dict[str, Callable[[EvalTarget], dict[str, str]]] = {
    "bug-fix-1": build_bug_fix_1_prompts,
    "dead-code": build_dead_code_prompts,
    "impact-analysis": build_impact_analysis_prompts,
    "feature-localization": build_feature_localization_prompts,
    "config-audit": build_config_audit_prompts,
    "migration": build_migration_prompts,
}


def build_prompts(eval_type: str, target: EvalTarget) -> dict[str, str]:
    """Public dispatch: return the 5-condition prompt dict for an eval type.

    Raises KeyError if the eval type isn't a diagnostic with codified prompts
    (e.g. `bug-fix` uses cloning and has its own prompt builder in
    `bug_fix.py`; `explain-repo` and `navigation-ctf` go through their own
    Click commands).
    """
    builder = BUILDERS[eval_type]
    return builder(target)
