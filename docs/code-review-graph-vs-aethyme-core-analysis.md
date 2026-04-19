# Analysis: `tirth8205/code-review-graph` vs Aethyme Core tools

Date: 2026-04-18

## Scope and method

This analysis compares:

1. The public `tirth8205/code-review-graph` repository (GitHub + raw file reads).
2. Aethyme Core surfaces in this monorepo (CLI, API, architecture docs, scorecard engine, and skill guidance).

Because direct `git clone` to GitHub returned a 403 tunnel error in this environment, the external analysis is based on GitHub-rendered pages and raw file URLs.

### Why the 403 happens here

The exact clone failure was: `CONNECT tunnel failed, response 403` during HTTPS access to GitHub.
This usually indicates the runtime egress proxy/firewall denied the HTTP CONNECT tunnel for `github.com`, rather than a repository-level permission problem.

In other words, this is most likely an environment network policy issue (proxy ACL, corporate firewall rule, or outbound restriction), not evidence that `tirth8205/code-review-graph` itself is private or blocked by GitHub auth.

Typical ways to validate in a less restricted environment:
- `git ls-remote https://github.com/tirth8205/code-review-graph.git`
- `curl -I https://github.com/tirth8205/code-review-graph`
- retry from a network without the same outbound proxy restrictions

## External repository snapshot (`code-review-graph`)

Primary observed claims and surfaces:

- Purpose: persistent, incremental local code knowledge graph for AI-assisted code review.
- Positioning: token reduction (headline claims include 6.8× and up to 49× reductions depending on workload).
- Parser/indexing: Tree-sitter + language packs.
- Runtime shape: Python package + FastMCP server + CLI.
- Tooling model: a large MCP toolset exposed to AI coding assistants.

Notable details observed from repository pages and files:

- Project metadata shows package `code-review-graph` at version `2.3.2` with Python `>=3.10` and core deps like `fastmcp`, `tree-sitter`, `networkx`, `watchdog`.
- CLI includes install/build/update/watch/status/visualize/wiki/detect-changes/repo registry/eval/serve paths.
- MCP server (`main.py`) registers a broad tool catalog, including:
  - build/update graph
  - minimal context and review context
  - impact radius and graph query/traversal
  - semantic search + embeddings
  - communities, flows, architecture overview
  - refactor preview/apply
  - wiki generation and cross-repo search

## Aethyme Core snapshot

Aethyme Core self-definition and architecture:

- Backend product in this repository.
- Responsibilities include indexing, graph persistence/traversal, search/ego/impact analysis, scorecard analysis, controlled autofixing, deterministic navigation primitives, and evaluation benchmarks.
- Canonical model is explicit multi-tenant hierarchy (`Platform > Org > Tenant > Repository > Graph`).
- Strategic split: Python for orchestration/delivery and Rust for deterministic engine kernels.

Observed delivery surfaces:

- **API** (FastAPI): health + search + ego + impact + scorecard routes, plus middleware for auth, CORS, trusted hosts, rate limiting, and optional metrics mount.
- **CLI** (`aethyme`): rich local workflow across repo ingestion/inspection, query/graph/task families, evaluation suites, AI-readiness scan, and autofix flow.
- **Skill guidance**: local navigation skill oriented around Rust engine commands (`query-areas`, `deps`, `importers`, `callers`, `query-overview`).
- **Scorecard engine**: detector-based scan framework with weighted findings and report export.

## Side-by-side comparison

## 1) Product intent

- **code-review-graph**: sharply optimized for AI code-review context compression and assistant integration via MCP.
- **Aethyme Core**: broader platform substrate spanning indexing + graph navigation + eval orchestration + scorecard governance + API auth boundaries.

## 2) Runtime architecture

- **code-review-graph**: Python-centric (CLI + MCP server) with local graph persistence and tooling around incremental updates.
- **Aethyme Core**: deliberate Python+Rust split where deterministic engines migrate into Rust while Python stays as orchestration/API/CLI envelope.

## 3) Tool surface breadth vs governance depth

- **code-review-graph** emphasizes breadth of MCP-facing analysis tools for direct assistant consumption (including refactor and wiki generation).
- **Aethyme** emphasizes governance and enterprise-readiness traits: tenant isolation, auth boundaries, API contracts, controlled autofix mechanisms, scorecard policy signal.

## 4) Interaction model

- **code-review-graph**: “assistant-first” with direct MCP tools intended to be called continuously during coding/review.
- **Aethyme**: “engine-first + product surface” model where CLI/API/eval pipelines are explicit and deterministic, with skill usage as guided navigation rather than only agent autopilot.

## 5) Evaluation philosophy

- **code-review-graph**: benchmark framing is token efficiency and review-context minimization.
- **Aethyme**: evaluation framework includes explain-repo and navigation CTF style runs, with explicit control/explore/leverage comparisons and generated run artifacts.

## 6) Search and graph analysis

- **Both** provide graph-oriented impact and dependency analysis.
- **code-review-graph** appears stronger on immediate assistant-consumable contextual payloads (`minimal_context`, `review_context`) and community/flow analytics in one MCP package.
- **Aethyme** currently shows stronger formal product boundary handling (auth, tenancy, rate limiting, API structure) and deterministic-engine roadmap depth.

## 7) Packaging and adoption path

- **code-review-graph**: low-friction install and platform auto-configuration for multiple coding assistants.
- **Aethyme**: richer internal platform scope; may require clearer “single-command quick start” parity for externally comparable adoption speed.

## Competitive takeaways for Aethyme Core tools

1. **What Aethyme is already stronger at**
   - Multi-tenant and auth-aware system boundary design.
   - Structured API surface suitable for controlled deployments.
   - Deterministic engine migration strategy (Rust focus) rather than pure scripting convenience.
   - Built-in scorecard and controlled autofix governance concepts.

2. **Where code-review-graph currently sets the UX bar**
   - MCP install ergonomics and broad cross-assistant integration language.
   - Highly explicit tool naming for agent action selection.
   - Tight narrative around token ROI with benchmark storytelling.

3. **Likely highest-impact improvements for Aethyme parity/advantage**
   - Add/standardize a compact MCP “minimal_context” + “review_context” pair aligned to Aethyme’s deterministic pack system.
   - Publish one-click onboarding for mainstream AI coding clients (Codex/Claude/Cursor/etc.) with consistent config generation.
   - Expose scorecard and policy signals as first-class context returned by review tools (not only separate scans).
   - Ship comparable reproducible benchmark scripts for token reduction and review quality deltas with public summary artifacts.


## Why `code-review-graph` can claim much larger token reductions than strict Aethyme evals

The short answer is: **the measurement setup is very different**, and Aethyme intentionally uses harder integrity constraints.

1. **Aethyme eval design is adversarial to vanity gains**
   - Aethyme explicitly forbids making eval-specific tool or prompt changes just to increase scores.
   - This prevents overfitting to one benchmark shape and often lowers headline metrics compared with highly optimized demos.

2. **Aethyme evaluates multiple conditions, not only best-case assisted runs**
   - The protocol requires four conditions (`control-cto-off`, `control-cto-on`, `explore`, `leverage`) to isolate baseline, terminal compression, and graph-assistance effects.
   - Public “X× token reduction” claims in other projects are often reported from one favorable assisted condition versus a weaker baseline.

3. **Aethyme separates CTO terminal compression from graph value**
   - Aethyme explicitly toggles Chau7 Command Token Optimization (CTO) to prevent mixing terminal-output compression benefits with graph-navigation benefits.
   - If another benchmark conflates these, token savings can appear dramatically larger.

4. **Aethyme captures full run telemetry (including sub-agents)**
   - The protocol requires storing full transcripts, tool calls, stdout/stderr, and summing parent + delegated sub-agent token usage.
   - Many public claims undercount by omitting delegation or hidden tool-chain usage.

5. **Task scope differs: strict structured outputs vs lightweight context fetches**
   - Aethyme explain/navigation evals require robust structured deliverables and evidence fields.
   - If a competing benchmark optimizes for minimal review-context extraction only, token totals can be much lower by design.

6. **Aethyme optimizes for generality, not one KPI**
   - The architecture and protocol favor broad reliability across repos/tasks.
   - Systems optimized for a single KPI (token compression) can outperform on that KPI while being less representative of full-task quality or generalization.

### Practical implication

If you compare only equivalent slices (same task contract, same output schema, same delegation accounting, same compression settings), the gap should shrink. The current headline difference is likely mostly a **benchmark-design delta**, not proof that Aethyme's graph is intrinsically weaker.


## What specifically makes `code-review-graph` feel "great" in practice (code-informed)

After reviewing the repository code paths (not just README messaging), the strongest differentiators are mostly **productized ergonomics + agent interface design**, not a single secret algorithm.

### 1) It is aggressively assistant-native at the API boundary

- The MCP server is first-class and declares a broad, explicit tool surface (`@mcp.tool()` wrappers) for build/update, review context, architecture, communities, flows, risk analysis, refactor, wiki, and cross-repo queries.
- It also exposes MCP prompt workflows (`@mcp.prompt()`) like review changes, architecture mapping, debugging, onboarding, and pre-merge checks.

**Why this matters:** assistants work better when tools are numerous, explicit, and semantically named. The repo invests heavily in that API design.

### 2) It bakes token-aware response shaping into shared helpers

- A common helper (`compact_response`) standardizes short responses and conditionally includes rich payloads only outside minimal mode.
- Multiple tools support `detail_level` with a minimal path, so the system can return quick summaries when the model doesn't need full data.

**Why this matters:** this creates consistent low-token behavior across many tools, not just one endpoint.

### 3) Incremental-first graph lifecycle is deeply integrated

- Build/update defaults to incremental workflows and includes postprocess levels.
- Incremental plumbing uses git diff detection with safe ref validation and tracked-file preference (`git ls-files`) with file-system fallback.

**Why this matters:** low latency and low cost are operational defaults rather than optional optimizations.

### 4) Storage model is simple and fast for local loops

- SQLite-backed graph store with explicit indexes on high-frequency query dimensions (file, kind, qualified symbol, source/target edge lookups).
- Structured node/edge dataclasses include enough metadata (types, file ranges, confidence tier, file hash) to support impact and traversal quickly.

**Why this matters:** the architecture trades cloud complexity for very fast local feedback, which users experience as “snappy” and reliable.

### 5) The review engine is actionable, not only descriptive

- Change analysis returns risk score, affected flows, test gaps, and priority ordering for changed functions/classes.
- The review path computes impact radius first, then derives compact risk-level summaries in minimal mode.

**Why this matters:** users get immediate “what to review next” output, which is exactly what code review workflows need.

### 6) It blends graph intelligence with architecture intelligence

- Beyond callers/callees, it ships high-level architecture tools (communities, bridge nodes, surprising connections, knowledge gaps, suggested questions).
- It even documents approximation choices (e.g., sampled centrality for large graphs), which indicates practical scaling work.

**Why this matters:** the tool helps with both micro (changed functions) and macro (system coupling) understanding in one flow.

### 7) It prioritizes install-time adoption friction

- CLI emphasizes one-command install + platform auto-configuration and injects usage instructions into assistant/platform config files.

**Why this matters:** adoption speed is often the biggest predictor of perceived product quality; this project treats onboarding as a core feature.

## Bottom line

`code-review-graph` feels strong because it combines:

1. a fast local graph engine,
2. a wide assistant-facing MCP interface,
3. consistent token-minimal output shaping,
4. and very polished onboarding defaults.

So its advantage is less “one magical parser trick” and more **tight end-to-end product execution around the assistant workflow loop**.


## Use-case coverage: is `code-review-graph` only for code review?

Short answer: **No**. Code review is the headline use case, but the tool surface clearly covers a broader assistant workflow.

### Covered use cases (from tool/prompt surface)

1. **PR / change review (primary)**
   - review context for changed files/functions
   - impact radius and risk-oriented prioritization

2. **Repository onboarding / understanding**
   - minimal context and architecture overviews
   - graph neighborhood/traversal for unfamiliar code areas

3. **Debugging and incident triage**
   - flow/path analysis and dependency tracing
   - “what changed / what is affected” style queries

4. **Refactoring planning/execution**
   - refactor preview + apply style capabilities
   - relationship-aware navigation to avoid blind edits

5. **Architecture and coupling analysis**
   - communities, bridge nodes, surprising connections, knowledge gaps
   - system-level structural reasoning beyond file-by-file grep

6. **Knowledge capture / docs generation**
   - wiki generation and structured context extraction
   - reusable project knowledge for future agent sessions

7. **Multi-repo discovery (where configured)**
   - cross-repo search and context lookup for federated codebases

### Practical interpretation

So the product should be thought of as a **local graph-backed assistant co-pilot for software maintenance workflows** (review + debug + refactor + onboarding + architecture), not a single-purpose review bot.

## Aethyme parity plan (prioritized)

This plan is designed to match the strongest experiential advantages quickly while preserving Aethyme’s governance and deterministic architecture strengths.

### Phase 0 (1-2 weeks): Instrumentation parity and benchmark alignment

**Goal:** ensure apples-to-apples measurement before feature race.

- Create a benchmark profile that mirrors their likely usage slice:
  - short-horizon review task
  - minimal response mode
  - no schema-heavy output requirements
- Record deltas against existing strict protocol (current default remains strict).
- Publish dual reporting in eval artifacts:
  - `strict_protocol_metrics`
  - `assistant_loop_metrics`

**Success criteria:** Aethyme can report both rigorous and UX-oriented numbers without mixing them.

### Phase 1 (2-4 weeks): Assistant-facing minimal context primitives

**Goal:** match “instant useful output” for agent loops.

Ship two first-class commands/tools (CLI + MCP wrapper):

1. `aethyme review minimal-context`
   - inputs: repo, changed files/commit range
   - output: compact summary, top impacted symbols, priority review order

2. `aethyme review review-context`
   - inputs: same + detail level
   - output: richer change graph, likely risk areas, candidate tests

Design rules:
- deterministic core computation
- strict token budgets with tiered `detail_level`
- compact default output

**Success criteria:** median assistant turn can call one command and get actionable review guidance in <2 tool calls.

### Phase 2 (2-4 weeks): Onboarding and install UX parity

**Goal:** reduce setup friction to near-zero.

- Add one-command assistant setup:
  - `aethyme setup codex`
  - `aethyme setup claude`
  - `aethyme setup cursor`
- Include:
  - tool registration
  - minimal usage instructions
  - safe overwrite/rollback
- Add health check command:
  - `aethyme doctor`

**Success criteria:** new user can install + run first context query in <5 minutes.

### Phase 3 (3-5 weeks): Actionability layer for review/debug tasks

**Goal:** move from “graph data” to “next action”.

- Add risk scoring bundle for changed code:
  - blast radius
  - coupling hotspots
  - config/entrypoint touch indicators
  - probable test impact
- Add ranked recommendations:
  - “review these 5 nodes first”
  - “run these test subsets first”

**Success criteria:** output includes explicit ranked actions, not just graph facts.

### Phase 4 (4-6 weeks): Architecture intelligence pack

**Goal:** close macro-understanding gap.

- Add stable architecture tools:
  - component communities
  - bridge/chokepoint nodes
  - unexpected coupling edges
  - missing-doc / low-confidence zones
- Return both machine JSON + compact human summary.

**Success criteria:** one command can produce both architecture snapshot and actionable risk map.

### Phase 5 (ongoing): Differentiation, not only parity

**Goal:** leverage Aethyme-native strengths.

- Expose scorecard/policy signals directly in review-context outputs.
- Keep tenant/auth policy constraints as first-class output metadata.
- Preserve strict eval protocol as the canonical scientific benchmark.

**Success criteria:** Aethyme is not just equivalent UX; it is better for governed and enterprise contexts.

### Implementation notes

- Build thin wrappers at the interface layer; keep core graph computation in deterministic engine paths.
- Do not relax strict protocol; add a second UX benchmark track rather than replacing the rigorous one.
- Keep every new “assistant loop” endpoint traceable to existing graph/eval primitives for debuggability.

## Conclusion

`code-review-graph` and Aethyme overlap in graph-powered repository reasoning but diverge in center of gravity:

- `code-review-graph` is optimized for immediate assistant productivity and token efficiency in local development loops.
- Aethyme Core is building a deeper long-term substrate for deterministic, governed, and multi-tenant code intelligence workflows.

Aethyme’s best strategic move is not to copy every MCP tool 1:1, but to combine its governance + deterministic-engine strengths with a thinner, more ergonomic assistant-facing interface that feels as immediate as `code-review-graph` while remaining policy-aware.

## External sources used

- GitHub repo page: https://github.com/tirth8205/code-review-graph
- Raw `pyproject.toml`: https://raw.githubusercontent.com/tirth8205/code-review-graph/main/pyproject.toml
- Raw `code_review_graph/cli.py`: https://raw.githubusercontent.com/tirth8205/code-review-graph/main/code_review_graph/cli.py
- Raw `code_review_graph/main.py`: https://raw.githubusercontent.com/tirth8205/code-review-graph/main/code_review_graph/main.py
