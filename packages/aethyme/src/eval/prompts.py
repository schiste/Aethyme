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
from .tools.manifest import ToolManifest, _substitute_prompt_text

CONDITION_NAMES: tuple[str, ...] = (
    "control-cto-off",
    "control-cto-on",
    "explore",
    "leverage",
    "task-conditioned",
)


# The leverage and task-conditioned conditions get a *minimal* prompt
# pointer at the active tool — not a step-by-step guide. The whole
# point of the leverage-vs-explore comparison is to measure the cost of
# "agent told the tool exists" vs "agent has skill loaded but no
# instruction." An earlier hardcoded version named the canonical
# Aethyme intent (`usage_boundary_query`, `behavior_localization_query`)
# and listed the response fields to read. That biased the
# discoverability gap upward — we were measuring "great prompt
# engineering" rather than "tool was pointed at." The current shape
# just names the entry point; the agent must read SKILL.md (or
# experiment) to learn how to invoke it. That's the honest measurement.
#
# The hint templates now live in each tool's manifest under [prompts]
# (see src/eval/tools/manifest.py and evals/tools/*.toml). This module
# pulls them out via _resolve_hints and substitutes {{TOOL_NAME}} /
# {{SKILL_PATH}}. {{TOOL_NAME}} binds to manifest.display_name, not the
# slug — closest to the old hardcoded "Aethyme is available..."
# capitalization, minimizing methodology drift on the casing axis.


def _resolve_hints(manifest: ToolManifest) -> tuple[str, str]:
    """Substitute manifest prompt templates into ready-to-emit hint strings.

    Returns ``(leverage_hint, task_conditioned_hint)``, each with the
    trailing ``\\n\\n`` separator the prompt assembler expects. The
    trailing whitespace is appended here, not in the TOML template —
    centralizing it makes it impossible for a manifest author to forget.

    Raises ``ValueError`` if the manifest declares ``leverage`` or
    ``task-conditioned`` conditions but lacks a ``[prompts]`` table.
    (The manifest loader normally catches this; the check here is a
    second line of defense if a hand-constructed ``ToolManifest``
    sneaks past validation.)
    """
    if manifest.prompts is None:
        raise ValueError(
            f"Manifest {manifest.name!r} has no [prompts] table; cannot "
            "render leverage / task-conditioned hints. Add a [prompts] "
            "table or remove the leverage / task-conditioned conditions."
        )
    leverage = _substitute_prompt_text(
        manifest.prompts.leverage_hint,
        tool_name=manifest.display_name,
        skill_path=manifest.prompts.skill_path,
    )
    task_conditioned = _substitute_prompt_text(
        manifest.prompts.task_conditioned_hint,
        tool_name=manifest.display_name,
        skill_path=manifest.prompts.skill_path,
    )
    return (leverage + "\n\n", task_conditioned + "\n\n")


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
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    """Standard 5-condition prompt assembly.

    Both hints are required keyword arguments now — they come from
    ``_resolve_hints(manifest)`` upstream and carry their trailing
    ``\\n\\n``. The control-* and explore conditions ignore the hints
    by design (those conditions test discoverability without a prompt
    addendum).
    """
    out: dict[str, str] = {}
    for cond in CONDITION_NAMES:
        prefix = ""
        if cond == "leverage":
            prefix = leverage_hint
        elif cond == "task-conditioned":
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


def build_bug_fix_1_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    """T419918 watchlist diagnostic. MediaWiki-only."""
    return _build_per_condition(
        target=target,
        task=_BUG_FIX_1_TASK,
        schema_shape=_BUG_FIX_1_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
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


def build_dead_code_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_DEAD_CODE_TASK,
        schema_shape=_DEAD_CODE_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
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


def build_impact_analysis_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_IMPACT_ANALYSIS_TASK,
        schema_shape=_IMPACT_ANALYSIS_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
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


def build_feature_localization_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_FEATURE_LOCALIZATION_TASK,
        schema_shape=_FEATURE_LOCALIZATION_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
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


def build_config_audit_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_CONFIG_AUDIT_TASK,
        schema_shape=_CONFIG_AUDIT_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
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


def build_migration_prompts(
    target: EvalTarget,
    *,
    leverage_hint: str,
    task_conditioned_hint: str,
) -> dict[str, str]:
    return _build_per_condition(
        target=target,
        task=_MIGRATION_TASK,
        schema_shape=_MIGRATION_SCHEMA_SHAPE,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
    )


# ---------------------------------------------------------------------------
# Registry: lookup table the orchestrator uses.
# ---------------------------------------------------------------------------


# Builder signature is `(target, *, leverage_hint, task_conditioned_hint) ->
# dict[str, str]`. `Callable[..., dict[str, str]]` is the simplest typing
# that admits keyword-only parameters — PEP 612's ParamSpec would be more
# precise but overkill for an internal lookup table.
BUILDERS: dict[str, Callable[..., dict[str, str]]] = {
    "bug-fix-1": build_bug_fix_1_prompts,
    "dead-code": build_dead_code_prompts,
    "impact-analysis": build_impact_analysis_prompts,
    "feature-localization": build_feature_localization_prompts,
    "config-audit": build_config_audit_prompts,
    "migration": build_migration_prompts,
}


def build_prompts(
    eval_type: str,
    target: EvalTarget,
    manifest: ToolManifest,
) -> dict[str, str]:
    """Public dispatch: return the 5-condition prompt dict for an eval type.

    ``manifest`` supplies the leverage / task-conditioned hint templates;
    {{TOOL_NAME}} is bound to ``manifest.display_name`` and {{SKILL_PATH}}
    to ``manifest.prompts.skill_path``. The hint substitution happens once
    here, then both resolved strings are forwarded to the per-eval-type
    builder.

    Raises KeyError if the eval type isn't a diagnostic with codified prompts
    (e.g. `bug-fix` uses cloning and has its own prompt builder in
    `bug_fix.py`; `explain-repo` and `navigation-ctf` go through their own
    Click commands). Raises ValueError via ``_resolve_hints`` if the
    manifest has no ``[prompts]`` table.
    """
    leverage_hint, task_conditioned_hint = _resolve_hints(manifest)
    builder = BUILDERS[eval_type]
    return builder(
        target,
        leverage_hint=leverage_hint,
        task_conditioned_hint=task_conditioned_hint,
    )
