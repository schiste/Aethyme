"""Framework self-identity — which tool is AethymeBench's primary subject.

AethymeBench was originally built to benchmark *Aethyme*, the tool, so the
literal string ``"aethyme"`` ended up hardcoded in five-plus places across
the eval harness — wherever the framework had to ask "is the current tool
the one we treat as our own?" (structured nav-context vs tool-context-file
pointer, negative-context plausibility flow, prompt preamble shape).

This module centralizes that concept. Three concerns that *look* the same
but aren't:

1. **Tool identity**: the ``name`` field in ``evals/tools/<tool>.toml``.
   Owned by :mod:`src.eval.tools.registry`. Plural — any tool registered
   in the manifest dir has an identity.
2. **Per-target default tool**: which tool a specific
   :class:`~src.eval.targets.EvalTarget` prefers when ``--tool`` isn't
   passed. Owned by ``EvalTarget.default_tool``.
3. **Framework self-tool** (this module): which tool the *framework
   itself* treats as its primary subject. Singular. Decides which adapter
   gets the "deep integration" flow (Aethyme-shape structured JSON,
   negative-context plausibility checks) vs the "generic" flow (tool-
   context-file pointer + auto-skip semantics).

Concerns 1 and 2 are tool-side; concern 3 is framework-side. Forks that
extract AethymeBench and rename their primary subject (e.g. a hypothetical
``graphbench`` benchmarking Graphify first) set
``AETHYMEBENCH_SELF_TOOL=graphify`` once and the five-plus call sites
across :mod:`src.eval.bug_fix`, :mod:`src.eval.explain_repo`, and
:mod:`src.eval.orchestrator` re-route automatically.

Kept deliberately tiny and dependency-free so it can be imported from
:mod:`src.eval.targets` (where the per-target default factory needs it)
without inducing an import cycle through :mod:`src.eval.tools`.
"""

from __future__ import annotations

import os

_SELF_TOOL_ENV_VAR = "AETHYMEBENCH_SELF_TOOL"
_DEFAULT_SELF_TOOL = "aethyme"


def self_tool_name() -> str:
    """Return the framework's self-tool name.

    Reads ``AETHYMEBENCH_SELF_TOOL`` from the environment; falls back to
    ``"aethyme"`` (AethymeBench's historical and default subject).

    Whitespace is stripped; an empty / whitespace-only override is
    treated as unset (returns the default) so a misconfigured shell
    can't degrade evals into running the "non-self-tool" flow by
    accident.
    """
    override = os.environ.get(_SELF_TOOL_ENV_VAR, "").strip()
    return override or _DEFAULT_SELF_TOOL


def is_self_tool(tool_name: str | None) -> bool:
    """Return True if ``tool_name`` is the framework's self-tool.

    ``None`` is treated as self-tool too: the framework's pre-Stage-B
    convention is that "no tool specified" means "run against the
    self-tool". Callers that want strict matching should compare to
    :func:`self_tool_name` directly.
    """
    if tool_name is None:
        return True
    return tool_name == self_tool_name()
