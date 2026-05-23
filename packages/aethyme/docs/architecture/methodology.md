# AethymeBench methodology

This document is the canonical specification of what running the
AethymeBench eval framework *means*. It is the referent for the
`methodology_hash` (item 2.16 in [`extraction-plan.md`](extraction-plan.md))
and the contract every adopter — Aethyme, Graphify, or any future tool —
must satisfy to claim that their results were produced "under
AethymeBench protocol."

The doc is deliberately a *referent*, not a re-exposition. Where another
file in the repo is already authoritative (the parity-test contract, the
eval protocol, the orchestrator's condition definitions), this doc names
and links rather than restating. The single-source-of-truth invariant is
load-bearing: any prose duplicated here would silently drift from the
code it claims to specify.

## 1. Scope: what AethymeBench measures

AethymeBench measures **whether a code-navigation tool changes how an LLM
agent solves a repository-grounded task** — and at what cost — relative
to the same agent without that tool. The unit of comparison is an *eval
type × target repo × tool* run across a fixed condition matrix; the
output is a per-condition table of quality, recall, cost, latency, and
tool-call counts plus a diagnostic narrative.

What AethymeBench is *not* measuring:

- It is not a benchmark of LLM capability. The agent model is held
  constant within a comparison; cross-model numbers are not directly
  comparable and the framework does not pretend otherwise.
- It is not a measure of "the best tool." A tool's score depends on the
  eval type, the target repo, and the condition under test. Results are
  diagnostic across this matrix, not a single ranking.
- It is not a synthetic benchmark. Every target repo is a real codebase
  (currently the Mockup TypeScript monorepo and a MediaWiki PHP
  monolith — see [`src/eval/targets.py`](../../src/eval/targets.py)).
  Tasks are real diagnostics drawn from those repos' actual histories
  (e.g. MediaWiki bug T419918).

## 2. The condition matrix

Every eval run executes the same agent prompt across a fixed set of
conditions that vary only by what tooling / pre-computed context the
agent has access to. This is the framework's core experimental design.

### 2.1 The five default conditions

Authoritative definition: [`src/eval/orchestrator.py:122-138`](../../src/eval/orchestrator.py).

| # | Name | Repo | CTO | Tool | Prompt variant | What it isolates |
|---|---|---|---|---|---|---|
| 1 | `control-cto-off` | Control clone | `forceOff` | none | baseline | Agent solves with stock Claude Code, no Aethyme-style tool, no command-token-optimization. The scientific zero. |
| 2 | `control-cto-on` | Control clone | default | none | baseline | Same as #1 but with Claude Code's CTO turned on. Isolates the CTO effect from the navigation-tool effect. |
| 3 | `explore` | Tool clone | default | available via skill | baseline | Agent has the tool installed in-repo and *may* discover it via Claude Code's skill / CLAUDE.md surface. Tests whether the tool's discoverability is sufficient. |
| 4 | `leverage` | Tool clone | default | invoked + result pasted | leverage | Agent is told the tool exists and is given the tool's pre-computed task-pack output (anchors / scope / relationships, no file bodies) as a prompt addendum. Tests the *payload* in isolation from discoverability cost. |
| 5 | `task-conditioned` | Tool clone | default | invoked + result pasted | task-conditioned | Same as #4 but the prompt addendum includes file bodies up to a 40k-byte budget. Tests whether handing the agent the full reading-list payoff up front changes its trajectory. |

The `CTO` column refers to Claude Code's *command token optimization*
setting (see [`feedback_eval_cto_settings.md`](../../../../.claude/projects/-Users-christophehenner-Downloads-Repositories-Aethyme/memory/feedback_eval_cto_settings.md)).
Only `control-cto-off` forces it off; conditions 3–5 use the default
(which is normally on) so that the Aethyme-vs-control comparison is
*Aethyme-with-CTO-on* vs *control-with-CTO-on* — the apples-to-apples
deployment shape, not an artificially weakened control.

### 2.2 The opt-in sixth condition: `negative-context`

Authoritative definition: [`src/eval/orchestrator.py:128-138`](../../src/eval/orchestrator.py).
Gating logic: [`src/eval/orchestrator.py:141-167`](../../src/eval/orchestrator.py).

A sixth condition, `negative-context`, runs only when an eval type
declares an `alternative_task` in its `_EVAL_TYPE_DEFAULTS` entry. It
pastes the leverage payload generated against a *sibling* task in the
same module — a *plausibly-wrong* nav-context blob — and lets the agent
proceed. It isolates **loading cost** (what `leverage` always pays to
ingest a blob) from **misdirection cost** (what an agent loses by
trusting wrong content).

The condition is opt-in because the diagnostic-eval flow (the prompts
written by [`src/eval/prompts.py`](../../src/eval/prompts.py) — `dead-code`,
`bug-fix-1`, `impact-analysis`, etc.) does not currently generate a
nav-context file; only `bug_fix.py` does. Eval types without an
`alternative_task` run with the 5 default conditions and `report.py`
records the omission.

For non-Aethyme tools (`graphify`, future adopters) the condition
degenerates per item 2.17 in `extraction-plan.md`. The current behaviour
is a leverage replay with `negative_context_status` reflecting that no
genuine wrong-content artifact was generated; this is documented as an
asymmetry, not hidden. Item 2.17 will stake out the cleaner answer
(skip vs. replay) and codify the choice.

### 2.3 The "always 5" rule, in this doc's terms

Every comparable AethymeBench result includes results for the five
default conditions. Truncating to a subset is methodologically
permitted only as a debug-mode affordance (`--conditions
control-cto-off,leverage`) and must not appear in any published or
cited comparison. Published numbers always list every condition, with
`null` / "not run" if some condition was intentionally skipped (e.g. a
tool that declares no negative-context implementation).

## 3. The tool-adapter contract

A tool is integrated into AethymeBench by writing a TOML manifest at
`evals/tools/<tool>.toml`. The loader is
[`src/eval/tools/manifest.py`](../../src/eval/tools/manifest.py); two
in-tree examples are
[`evals/tools/aethyme.toml`](../../evals/tools/aethyme.toml) and
[`evals/tools/graphify.toml`](../../evals/tools/graphify.toml).

The manifest specifies the tool's lifecycle (`install`, `register`,
`version`, `warm`) and how each of the three tool-using conditions
(`explore`, `leverage`, `task_conditioned`) maps to a shell command on
that tool. Two of those lifecycle / condition fields are themselves
load-bearing for methodology integrity, called out below.

### 3.1 The `[notes].condition_mapping` requirement

Every manifest **must** carry a non-empty `[notes].condition_mapping`
prose block. Enforcement is in
[`src/eval/tools/manifest.py:213-216`](../../src/eval/tools/manifest.py) —
the loader raises if the field is empty.

The block has one job: explain, in plain English, exactly how this
tool's commands map onto the five conditions and which asymmetries
exist relative to Aethyme. It is the audit trail for anyone reading
results downstream; without it, "this tool got 47.2 on Recall under
leverage" is uninterpretable because the reader doesn't know what
"leverage" was actually given to the agent.

Two reference examples:

- [`evals/tools/aethyme.toml` `[notes].condition_mapping`](../../evals/tools/aethyme.toml)
  documents that Aethyme's `task pack` (leverage) and `task context`
  (task-conditioned) are genuinely different payloads, and explains why
  the in-tree Python → CLI migration was a transport change, not a
  semantic one.
- [`evals/tools/graphify.toml` `[notes].condition_mapping`](../../evals/tools/graphify.toml)
  documents the explicit asymmetry that Graphify has only one query
  mode, so `leverage` and `task_conditioned` give *identical commands*
  but differ in prompt framing only — a fact that materially changes
  how results between those two columns should be read.

The block surfaces in the per-run output JSON as
`tool_manifest_notes` ([`src/eval/orchestrator.py:518`](../../src/eval/orchestrator.py)).
Reports cite it inline so downstream readers cannot lose the caveats.

### 3.2 The `in_tree=true` privilege (generalized via `AETHYMEBENCH_SELF_TOOL`)

A manifest may declare `[source].in_tree = true` to skip the
`git clone` step — the framework treats the host repo as the tool
source and resolves `{{TOOL_ROOT}}` to the package root. The framework
recognizes exactly one tool name as its self-tool, controlled by the
`AETHYMEBENCH_SELF_TOOL` environment variable (default `"aethyme"`).
A fork that develops the framework alongside its own tool sets
`AETHYMEBENCH_SELF_TOOL=<their-tool>` and gets the same iteration
affordance; the original `"aethyme"` name carries no special meaning
in code — every check goes through `src/eval/_self.py`'s
`is_self_tool(name)` / `self_tool_name()` helpers.

What the self-tool privilege actually grants:

- **Clone-step shortcut.** `in_tree=true` resolves `{{TOOL_ROOT}}` to
  the package root instead of a temp clone.
- **Structured nav-context flow.** Bug-fix, explain-repo, and
  navigation-CTF evals consume the self-tool's `leverage` /
  `task-conditioned` output as JSON (anchors, scope, file_contents)
  and render it into a structured `nav-context.json`. Non-self-tools
  take the tool-context-file flow: the adapter's output is written to
  `<repo>/.aethyme-eval-tool-context.md` and the leverage prompt
  points there, leaving the structured artifacts unrendered.
- **Run-dir slug elision.** The self-tool's slug is omitted from
  `eval-runs/{timestamp}-{target}-...-{tool}` names. A run of the
  self-tool produces a bare slug; competitor manifests always get an
  explicit `-graphify` / `-aethyme` suffix.

The privilege does *not* let the self-tool skip `install`, `register`,
`version`, or `warm` — those run on the in-tree path the same as on a
cloned path. The protocol is identical; only the "where does the code
live" question differs.

## 4. Parity-test discipline

**The contract: byte-identical, not functional.** Full specification:
[`parity-test-contract.md`](parity-test-contract.md).

Three local tests (`test_eval_navigation_context_adapter_parity.py`,
`test_eval_explain_repo_adapter_parity.py`,
`test_eval_navigation_ctf_adapter_parity.py`) assert that the legacy
direct-Python path and the tool-adapter CLI path produce
canonical-JSON-equal output (`json.dumps(value, sort_keys=True)`) on
every commit. A single divergent byte fails the test.

The contract relaxes only when item 2.3 in `extraction-plan.md`
executes — and even then, it relaxes by *swapping the reference*, not
by loosening the equality check. After 2.3, the tests assert
`adapter_output == golden_snapshot.json` instead of comparing live
implementations. The methodology-hash regime (item 2.16) makes the
golden snapshot itself first-class auditable.

Why byte-identical and not functional, in one line: any transport-level
drift that goes undetected confounds the cross-commit comparison that
is the whole point of publishing eval numbers. See
`parity-test-contract.md` §3 for the full argument.

## 5. Warm-cost surfacing

Tools differ in what their `warm` phase costs. The framework's
[`src/eval/orchestrator.py:_build_warm_phase`](../../src/eval/orchestrator.py)
runs each tool's `[warm].command` once before eval start; that cost is
*not* currently included in the per-condition cost metric. This is the
known asymmetry documented in
[`evals/tools/graphify.toml` `[notes].condition_mapping` caveat 1](../../evals/tools/graphify.toml):

> Graphify's `extract` step is LLM-coupled by design. For code-only
> target repos … we use `--no-cluster` which skips the LLM-driven
> cluster naming … For mixed-content repos (docs/PDFs/images) Graphify
> will invoke its configured backend; that cost is NOT currently
> captured by the eval framework's per-condition cost metric (it
> accrues to warm, which is currently unmeasured).

The methodologically clean fix is to capture warm-step cost as a
distinct line in the result JSON (per item 2.5 in
`extraction-plan.md`). Until then, every result that compares costs
across tools whose warm-phase work differs *must* call out the
unmeasured warm spend in its `tool_manifest_notes` rendering. This is
not a future improvement; it is a current contract on the report
generator and on anyone citing AethymeBench numbers.

The same rule applies to Aethyme. Aethyme's warm phase starts a Rust
engine daemon and waits for it to listen on a socket
([`evals/tools/aethyme.toml` `[warm].command`](../../evals/tools/aethyme.toml));
that startup cost is real and currently unmeasured. The framework does
not get a pass on its own asymmetries.

## 6. The four-section diagnostic-eval report

Authoritative implementation: [`src/eval/report.py:1004-1073`](../../src/eval/report.py).

Diagnostic eval types (`dead-code`, `bug-fix-1`, `impact-analysis`,
`feature-localization`, `config-audit`, `migration` — the frozenset
`DIAGNOSTIC_EVAL_TYPES` at line 1009) render under a 4-section narrative
scaffold rather than the bug-fix technical report. The four sections
are fixed:

1. **Overall results** — fully auto-derived. Headline comparison
   table, cost-effectiveness ranking, per-condition match details
   extracted from each scorer's `matched` / `missed` /
   `false_positives` arrays.
2. **General learnings** — auto-derived findings: CTO impact size,
   leverage-vs-control deltas, cache-cost dominance, any signals from
   divergent ranking by metric. Empty *interpretation* placeholders
   stay empty by design — the framework scaffolds the structure, a
   human reader draws the conclusions.
3. **Tooling layer focus** — auto-derived: tool-call counts per
   condition, any Chau7 / framework caveats that fired during the run
   (stale `session_id`s, shell-quoting edge cases, parallel-launch
   efficiency).
4. **Graph layer focus** — auto-derived from condition results: which
   targets only some conditions found (where the tool's graph caught
   something the controls missed), and where the graph failed (false
   positives shared across conditions).

The 4-section structure is not advisory — it is generated mechanically
by [`_render_diagnostic_markdown`](../../src/eval/report.py). Hand-written
markdown reports are forbidden by the
[standardized-reports rule](../../../../.claude/projects/-Users-christophehenner-Downloads-Repositories-Aethyme/memory/feedback_standardized_reports.md);
every published report must come out of the code path so the structure
itself is part of the methodology contract.

Bug-fix, explain-repo, and navigation-ctf evals continue to use the
older `_render_markdown` until item 2.12 (`EvalTypeAdapter` Protocol)
unifies the two flows.

## 7. Methodology versioning

The fields *named in this document* are the load-bearing methodology
surface. A change to any of them is a methodology change, not a refactor.
Item 2.16 in `extraction-plan.md` operationalizes that distinction:

- **`methodology_hash`** — a 12-character hex prefix of
  `sha256(canonical_methodology_inputs)`. The inputs are, in order:
  - The serialized condition matrix from `CONDITIONS` in
    [`src/eval/orchestrator.py`](../../src/eval/orchestrator.py).
  - The set of prompt templates (`src/eval/prompts.py` + each eval
    type's `task` / `objective` strings).
  - The set of scorer implementations (per-eval-type scoring code
    digests).
  - For each tool participating in the run, the SHA256 of its
    `evals/tools/<tool>.toml` after manifest normalization (commands +
    `[notes].condition_mapping` text included; whitespace-only diffs
    normalized).
  - The framework's CalVer version string (post-extraction).
- **Golden snapshot** — every run writes
  `docs/methodology/snapshots/<methodology_hash>.json` capturing the
  exact inputs the hash was computed over. This makes the hash
  *auditable* rather than an opaque fingerprint.
- **`aethyme-bench methodology diff <a> <b>`** — diffs two snapshot
  files and prints a human-readable explanation of *what changed*
  between methodology hashes, so a result-pair's incomparability has a
  citable explanation.

Every run output JSON stamps `methodology_hash` at the top level.
Cross-commit or cross-tool comparisons are only protocol-valid when
the two `methodology_hash` values match (or, if they differ, when the
`methodology diff` between them affects no field that the comparison
touches — and that judgment is *explicit*, not assumed).

Until item 2.16 lands, the framework's methodology version is *the git
commit SHA of this monorepo* — recorded in run metadata as
`aethyme_commit` ([`src/eval/report.py`](../../src/eval/report.py)
`get_aethyme_commit`). That is a coarser proxy than `methodology_hash`
but it preserves auditability of "which methodology produced which
numbers" until the precise hash is implemented.

## 8. Cardinal rules incorporated by reference

The methodology described here only stands if certain rules outside its
direct scope hold. The full statements live in
[`CLAUDE.md`](../../../../CLAUDE.md) (top-level) and the user's
persistent memory; the load-bearing ones for *methodology integrity*
are:

- **All evaluations run against Playground repositories, never against
  Aethyme itself.** Running an eval against the host repo confounds the
  in-tree tool's privilege with the eval's signal.
- **Never modify tools, engine, pipeline, or skills to improve eval
  scores.** Evals are diagnostics, not targets. See
  [`docs/guides/eval-protocol.md`](../guides/eval-protocol.md) for
  worked examples of allowed-vs-forbidden changes.
- **Control repos sacred.** Once cloned, a control repo is the
  scientific baseline. Any modification — including agent-side
  artifacts the agent dropped in violation of its prompt — invalidates
  every subsequent comparison using that clone. The orchestrator's
  `prepare` phase re-checks control integrity; a fail there blocks the
  run.
- **No direct LLM calls.** All agent invocations go through Chau7 MCP
  → terminal tab → `claude` / `codex` CLI. Direct SDK calls would
  bypass the per-condition cost-accounting and break the
  apples-to-apples comparison this whole framework relies on.

These rules are upstream of the methodology in this doc; violating any
of them invalidates the methodology hash regardless of what the hash
computes to.

## 9. Cross-references

- [`extraction-plan.md`](extraction-plan.md) — the parent plan;
  Stage B items 2.10 (this doc) and 2.16 (`methodology_hash`)
  are the methodology-integrity backbone.
- [`parity-test-contract.md`](parity-test-contract.md) — §4 above is a
  pointer to this; it is the authoritative spec for the byte-identical
  contract.
- [`docs/guides/eval-protocol.md`](../guides/eval-protocol.md) — the
  operational protocol (how to *run* an eval). This doc is the *why*;
  eval-protocol.md is the *how*.
- [`evals/tools/aethyme.toml`](../../evals/tools/aethyme.toml),
  [`evals/tools/graphify.toml`](../../evals/tools/graphify.toml) —
  the two reference manifests; their `[notes].condition_mapping`
  blocks are the in-the-wild examples of the §3.1 contract.
- [`src/eval/orchestrator.py`](../../src/eval/orchestrator.py)
  lines 122-167 — the condition matrix in code form; this doc's §2
  cites it as authoritative.
- [`src/eval/tools/manifest.py`](../../src/eval/tools/manifest.py)
  lines 213-216 — the enforcement point for `[notes].condition_mapping`
  being non-empty.
- [`src/eval/report.py`](../../src/eval/report.py) lines 1004-1073
  — the 4-section diagnostic report; §6 cites it as authoritative.
- [`CLAUDE.md`](../../../../CLAUDE.md) — Cardinal rules; §8 cites them
  as incorporated by reference.
