# Aethyme Core: Layered Agent Navigation Spec

Last Updated: 2026-03-06

## Purpose
This document is the canonical product and technical specification for Aethyme Core.

It defines:
- what Aethyme Core is trying to achieve
- the layered architecture of the product
- the phased implementation order
- the runtime and language split
- the primary product objects and evaluation model

This document is intentionally comprehensive. Shorter operational docs should point back to this one.

Research context for the major architectural decisions in this spec is captured in:
- [`architecture/research-informed-architecture-memo.md`](architecture/research-informed-architecture-memo.md)

## Executive Thesis
Aethyme Core exists to be the primary repository navigation layer for AI coding agents.

Its job is to make repository work:
- more deterministic
- more context-efficient
- lower-retry
- easier to review
- increasingly scope-aware
- eventually permission-aware

The product is not a generic code search system and not a generic agent platform.
The product is a deterministic substrate for agent navigation, context construction, scope shaping, and later agent performance management.

## Product Goal
The first real goal is not to solve every agent problem.
It is to make single-task code changes meaningfully better.

Aethyme should help agents:
1. find the right starting point
2. gather the smallest useful context
3. understand dependency and impact boundaries
4. avoid irrelevant or high-risk areas unless needed
5. operate in a smaller, more reviewable scope

## What Aethyme Core Is
Aethyme Core is:
- a repository mapping engine
- a discoverability layer over that map
- a context-pack builder for tasks
- a guidance layer for agent navigation
- later, a policy-aware execution substrate
- later, a foundation for deterministic agent management and performance management

## What Aethyme Core Is Not
Aethyme Core is not:
- a customer-facing identity product
- a generic SaaS shell
- a general-purpose observability platform
- a vague AI platform without a hard substrate
- a broad agent orchestration system in v1

## Product Boundary
### `packages/aethyme`
Owns:
- repository intake
- repository mapping
- discoverability and traversal
- context-pack construction
- scope and risk inference
- advisory guidance
- scorecard and repo quality signals
- local autofix/operator tooling
- auth enforcement and tenant isolation
- API and CLI adapters over the core engine

### `packages/aethyme-cloud`
Owns:
- identity issuance
- user, org, tenant, and admin lifecycle
- SaaS UX and management surfaces
- later agent management and policy UX
- later performance reporting UI

## Canonical Model
The canonical business hierarchy is:
- Platform
- Org
- Tenant
- Repository
- Graph

The canonical product hierarchy is:
- Repo Intake
- Mapping
- Discoverability
- Context
- Guidance
- Policy
- Action Support
- Evaluation
- Agent Management

The business hierarchy describes ownership and isolation.
The product hierarchy describes capabilities and implementation order.

## Design Principles
1. Deterministic before clever.
2. Minimal context before exhaustive context.
3. Repository facts before policy overlays.
4. Advisory guidance before hard enforcement.
5. Understanding before action.
6. Action support before management abstractions.
7. Measured improvement before product expansion.

## What "Deterministic" Means
For Aethyme, deterministic means:
- the same repo state and same task should produce the same or near-identical navigation object
- retrieval order should be stable
- scope decisions should be explainable
- pack construction should not rely on probabilistic generation in v1
- agents should need fewer open-ended searches and retries

## What "Efficient" Means
For Aethyme, efficiency is primarily about:
- fewer retries
- lower token usage
- lower human review burden

Secondary efficiency metrics include:
- lower wall-clock time
- fewer unnecessary file reads
- fewer irrelevant tool calls

## Primary Users
Primary users, in order, are:
1. coding agents
2. review agents
3. orchestration agents

The first product target is coding agents doing single-task code changes.

## Main Failure To Eliminate First
The first failure Aethyme should reduce is:
- agents pulling too much irrelevant context

That is the highest-leverage problem because it drives:
- token waste
- retries
- poor scope control
- broader diffs
- harder reviews

## Layered Architecture
Aethyme should be implemented as a layered system with clear outputs between layers.

| Layer | Goal | Main output | Primary owner |
|---|---|---|---|
| 0. Repo Intake | Normalize a repository | repo identity, snapshot, metadata | Python |
| 1. Mapping | Build structural knowledge | files, symbols, edges, metadata | Rust |
| 2. Discoverability | Make structure queryable | lookup, search, neighbors, impact | Rust |
| 3. Context | Build minimal task packs | anchors, snippets, frontier, scope | Rust |
| 4. Guidance | Shape agent navigation | read order, warnings, out-of-scope | Rust |
| 5. Policy | Apply advisory rules | risk overlays, escalation hints | Python first |
| 6. Action Support | Help bounded code changes | edit scope, blast radius, review pack | Mixed |
| 7. Evaluation | Measure product value | token, retry, review metrics | Python |
| 8. Agent Management | Control agent behavior at scale | profiles, permissions, routing | Cloud later |

### Why Layers
This layering separates four different concerns that must not collapse into one implementation:
- repository facts
- discoverability and navigation
- behavior shaping
- policy and management

Without this separation, the system will become hard to reason about and hard to evolve.

## Canonical Repograph Architecture
The repograph is the substrate beneath every higher layer.

It should not be treated as "search index plus a few edges."
The ultimate target is a layered, multi-resolution, function-centric repository graph with overlays for navigation, risk, policy, and performance.

### Core Principle
The graph should be:
- function-centric for code semantics
- file and area-centric for navigation
- documentation and config aware for explanation
- overlay-friendly for risk, policy, and task shaping

That means the repograph must answer all of these:
- what exists
- where it lives
- what it does
- what depends on it
- what explains it
- what configures it
- what is risky
- what should be read first
- what should not be touched

### Graph Layers
The final repograph should be modeled in distinct layers:

| Graph layer | Meaning | Examples |
|---|---|---|
| Structural truth | Hard repository facts | repo, area, directory, file, contains |
| Semantic truth | Code and runtime facts | function, class, imports, calls, references |
| Inferred meaning | Useful but probabilistic structure | entrypoints, representative areas, doc links |
| Control overlays | Human and system constraints | high-risk zones, policy rules, escalation requirements |
| Task overlays | Task-specific graph slices | anchors, in-scope, out-of-scope, context pack |
| Performance overlays | Measured execution data | token usage, retries, review burden, benchmark deltas |

These layers must stay separate.
Policy and task scope are not graph truth.

### Canonical Node Taxonomy
The ultimate node model should include:

#### Structural nodes
- `platform`
- `org`
- `tenant`
- `repo`
- `snapshot`
- `workspace`
- `area`
- `directory`
- `file`

#### Code semantic nodes
- `module`
- `namespace`
- `class`
- `interface`
- `trait`
- `struct`
- `enum`
- `function`
- `method`
- `field`
- `constant`
- `type_alias`
- `macro`
- `test_case`

#### Documentation nodes
- `doc`
- `doc_section`
- `decision_record`
- `runbook`
- `spec`
- `example`

#### Config and runtime nodes
- `config`
- `manifest`
- `dependency`
- `service`
- `runtime_target`
- `build_target`
- `pipeline`
- `environment`

#### Asset and content nodes
- `asset`
- `dataset`
- `schema`
- `template`
- `content_object`

#### Agent and task nodes
- `task`
- `context_pack`
- `navigation_step`
- `scope_boundary`
- `risk_zone`
- `policy_rule`
- `permission_request`

#### Execution and performance nodes
- `agent_profile`
- `agent_run`
- `tool_call`
- `patch`
- `review`
- `benchmark_case`
- `benchmark_run`
- `metric`

### Canonical Edge Taxonomy
The ultimate edge vocabulary should include:

#### Structural edges
- `contains`
- `belongs_to`
- `part_of`

#### Code semantic edges
- `defines`
- `declares`
- `imports`
- `exports`
- `references`
- `calls`
- `reads`
- `writes`
- `mutates`
- `constructs`
- `returns`
- `raises`
- `inherits`
- `implements`
- `specializes`
- `tests`
- `mocks`

#### Documentation and config edges
- `documents`
- `describes`
- `configures`
- `builds`
- `deploys`
- `depends_on`
- `uses`
- `generates`
- `transforms`

#### Navigation and scope edges
- `anchors`
- `in_scope_for`
- `out_of_scope_for`
- `entrypoint_for`
- `starting_point_for`
- `relevant_to`
- `irrelevant_to`

#### Control edges
- `high_risk_for`
- `restricted_for`
- `blocked_for`
- `requires_escalation_for`
- `allowed_for`

#### Execution and performance edges
- `used_in`
- `produced`
- `reviewed_by`
- `measured_by`
- `compared_to`

### Canonical Graph Unit
The smallest meaningful code node should be `function` or `method`.

The main navigation backbone should be:
- `repo -> area -> file -> function`

This is the core balance:
- function-level for change reasoning
- file and area-level for navigation, scope, and control

### Canonical Graph Record Shape
Every graph node should ultimately follow a normalized property-graph shape:

```ts
type GraphNode = {
  id: string
  kind: string
  label: string
  layer: "truth" | "semantic" | "inferred" | "control" | "task" | "performance"
  snapshot_id: string
  path?: string
  language?: string
  span?: {
    start_line: number
    end_line: number
    start_col?: number
    end_col?: number
  }
  confidence: number
  source: string
  metadata: Record<string, unknown>
}
```

Every graph edge should follow the same normalization discipline:

```ts
type GraphEdge = {
  id: string
  from: string
  to: string
  kind: string
  layer: "truth" | "semantic" | "inferred" | "control" | "task" | "performance"
  snapshot_id: string
  confidence: number
  source: string
  metadata: Record<string, unknown>
}
```

Annotations should remain explicit and separate:

```ts
type GraphAnnotation = {
  id: string
  target_id: string
  kind: string
  layer: "control" | "task" | "performance"
  snapshot_id: string
  confidence: number
  source: string
  value: string
  metadata: Record<string, unknown>
}
```

### Canonical Identity Strategy
IDs must be deterministic, snapshot-aware, and stable under rereads.

Examples:
- `repo:ADD`
- `snapshot:ADD:<commit-or-fingerprint>`
- `area:ADD:GameEngine`
- `file:ADD:GameEngine/rust/addgame/src/lib.rs`
- `fn:ADD:tools/osm_to_hexmap.py:build_hex_map`
- `doc:ADD:documentation/technical-architecture.md`
- `cfg:ADD:GameEngine/rust/addgame/Cargo.toml`
- `task:ADD:<task-hash>`
- `pack:ADD:<task-hash>:<snapshot-id>`

### Confidence Model
Confidence must exist from the start:
- `1.0` structural fact
- `0.95` compiler or indexer-resolved semantic fact
- `0.9` parser-derived semantic fact
- `0.75` strong inference
- `0.5` weak inference
- `0.25` advisory heuristic only

Higher layers must be able to distinguish truth from inference.

## Canonical Extraction Passes
The ultimate repograph should be built through explicit passes.

### Pass 0. Repo Intake
Creates the stable root context:
- repo id
- snapshot id
- canonical root
- build settings

Output artifact: `RepoContext`

### Pass 1. Filesystem Structure Extraction
Builds structural coverage of the entire repo.

Creates:
- `repo`
- `area` candidates
- `directory`
- `file`

Creates edges:
- `contains`

Output artifact: `StructureGraph`

### Pass 2. File Classification
Classifies each file and determines whether it should get deep parsing.

Output artifact: `ClassifiedFiles`

Primary roles:
- source
- test
- doc
- config
- manifest
- build
- asset
- generated
- binary
- cache

### Pass 3. Area Formation
Creates stable repo zones for navigation.

Output artifact: `AreaMap`

This pass assigns files and directories to:
- top-level areas
- later inferred subsystem areas

### Pass 4. Code Parsing
Runs language-specific parsers against supported source files.

Extracts:
- functions
- methods
- classes
- imports
- raw code relations

Output artifact: `ParsedCodeUnits`

### Pass 5. Symbol Normalization
Normalizes parser output into deterministic graph entities.

Creates:
- stable function nodes
- stable class nodes
- `defines` edges
- normalized imports

Output artifact: `NormalizedCodeGraph`

### Pass 6. Code Relationship Resolution
Resolves semantic relations between code entities.

Creates:
- `calls`
- `references`
- `inherits`
- `implements`
- `entrypoint_for` candidates later

Output artifact: `ResolvedCodeRelations`

### Pass 7. Documentation Extraction
Makes documentation first-class.

Creates:
- `doc`
- `doc_section` later

Creates edges:
- `documents`
- `belongs_to`
- `references`

Output artifact: `DocumentationGraph`

### Pass 8. Config / Manifest Extraction
Makes manifests and config files first-class.

Creates:
- `config`
- `manifest`

Creates edges:
- `configures`
- `depends_on`
- `entrypoint_for`

Output artifact: `ConfigGraph`

### Pass 9. Asset / Content Registration
Registers non-code repo material as graph entities.

Creates:
- `asset`
- later richer content nodes

Output artifact: `AssetGraph`

### Pass 10. Risk Annotation
Attaches high-risk overlays to graph entities.

Creates annotations for:
- auth
- permissions
- migrations
- infra
- secrets
- billing
- shared-core
- later user-defined risk zones

Output artifact: `RiskOverlay`

### Pass 11. Entry Point And Navigation Inference
Infers useful starting points and representative entities.

Creates:
- `entrypoint_for`
- `starting_point_for`
- representative docs, files, and functions

Output artifact: `NavigationOverlay`

### Pass 12. Graph Normalization
Sorts, deduplicates, validates, and stabilizes the graph.

Output artifact: `NormalizedRepositoryGraph`

### Pass 13. Persistence
Stores normalized nodes, edges, and annotations.

### Pass 14. Query And Navigation Views
Builds the graph-derived views needed by higher layers:
- repo overview
- area overview
- function neighborhood
- impact frontier
- task anchors
- context packs later

## Pass Data Contracts
Each pass should consume one explicit artifact and emit one explicit artifact.

### `RepoContext`
```ts
type RepoContext = {
  repo_id: string
  repo_path: string
  snapshot_id: string
  vcs?: {
    commit?: string
    dirty: boolean
  }
  settings: {
    include_hidden: boolean
    allowed_languages?: string[]
  }
}
```

### `StructureGraph`
```ts
type StructureGraph = {
  repo_context: RepoContext
  nodes: StructureNode[]
  edges: StructureEdge[]
}
```

### `ClassifiedFiles`
```ts
type ClassifiedFile = {
  file_id: string
  path: string
  language?: string
  role:
    | "source"
    | "test"
    | "doc"
    | "config"
    | "manifest"
    | "build"
    | "asset"
    | "generated"
    | "binary"
    | "cache"
  generated: boolean
  parseable: boolean
  metadata: Record<string, unknown>
}

type ClassifiedFiles = {
  repo_context: RepoContext
  files: ClassifiedFile[]
}
```

### `AreaMap`
```ts
type AreaMap = {
  repo_context: RepoContext
  areas: AreaNode[]
  assignments: {
    node_id: string
    area_id: string
    kind: "belongs_to"
    confidence: number
    source: string
  }[]
}
```

### `ParsedCodeUnits`
```ts
type ParsedCodeUnits = {
  repo_context: RepoContext
  classes: ParsedClass[]
  functions: ParsedFunction[]
  imports: ParsedImport[]
}
```

### `NormalizedCodeGraph`
```ts
type NormalizedCodeGraph = {
  repo_context: RepoContext
  classes: ClassNode[]
  functions: FunctionNode[]
  defines: DefinesEdge[]
  imports: GraphEdge[]
}
```

### `ResolvedCodeRelations`
```ts
type ResolvedCodeRelations = {
  repo_context: RepoContext
  relations: {
    from: string
    to: string
    kind: "calls" | "references" | "inherits" | "implements"
    confidence: number
    source: string
    metadata?: Record<string, unknown>
  }[]
}
```

### `DocumentationGraph`
```ts
type DocumentationGraph = {
  repo_context: RepoContext
  docs: DocNode[]
  relations: GraphEdge[]
}
```

### `ConfigGraph`
```ts
type ConfigGraph = {
  repo_context: RepoContext
  configs: ConfigNode[]
  relations: GraphEdge[]
}
```

### `AssetGraph`
```ts
type AssetGraph = {
  repo_context: RepoContext
  assets: AssetNode[]
}
```

### `RiskOverlay`
```ts
type RiskOverlay = {
  repo_context: RepoContext
  annotations: {
    target_id: string
    risk:
      | "auth"
      | "permissions"
      | "migrations"
      | "infra"
      | "secrets"
      | "billing"
      | "shared-core"
    level: "low" | "medium" | "high"
    confidence: number
    source: string
    reason: string
  }[]
}
```

### `NavigationOverlay`
```ts
type NavigationOverlay = {
  repo_context: RepoContext
  hints: {
    target_id: string
    kind: "entrypoint" | "overview" | "representative" | "starting_point"
    area_id?: string
    confidence: number
    source: string
    reason: string
  }[]
}
```

### `NormalizedRepositoryGraph`
```ts
type NormalizedRepositoryGraph = {
  repo_context: RepoContext
  nodes: GraphNode[]
  edges: GraphEdge[]
  annotations: GraphAnnotation[]
}
```

## Canonical Query Capabilities
The final repograph should support queries like:

### Repo navigation
- what are the main areas of this repo
- what are the representative entrypoints
- which docs explain each area

### Code change support
- which function actually implements this behavior
- who calls it
- what does it call
- what docs and configs shape it
- what breaks if it changes

### Scope and control
- what is in scope for this task
- what is likely out of scope
- which areas are high-risk
- what would later require escalation

### Performance
- did Aethyme reduce tokens on this benchmark
- where did the agent drift outside expected scope
- which graph signals correlated with retries or review burden

## Layer 0: Repo Intake
### Purpose
Turn a repository into a stable unit Aethyme can reason about.

### Owns
- repo registration
- revision identity
- local snapshot identity
- language detection
- config discovery
- ignore rules
- basic repo metadata

### Outputs
- repo id
- revision id
- language set
- path inventory
- config inventory

### Rules
This layer prepares the repo for mapping. It does not yet interpret tasks.

## Layer 1: Mapping
### Purpose
Produce deterministic structural facts about the repository.

### Owns
- file inventory
- symbol extraction
- imports and includes
- containment relationships
- call and reference edges where trustworthy
- structural metadata
- basic risk tagging from deterministic rules
- confidence labeling on items and edges

### Outputs
- file graph
- symbol graph
- edge set
- metadata set
- risk annotations
- confidence annotations

### Rules
This layer should produce facts, not task decisions.
Policy does not belong here.

## Layer 2: Discoverability
### Purpose
Make the mapping layer queryable and navigable.

### Owns
- symbol lookup
- file lookup
- path lookup
- fuzzy search
- dependency neighborhood expansion
- impact neighborhood expansion
- anchor candidate retrieval
- deterministic ranking of candidates

### Outputs
- ranked candidates
- dependency frontier
- impact frontier
- neighborhood sets
- anchor candidates

### Rules
This layer returns candidates and graph-adjacent results.
It still does not build task-context packs.

## Layer 3: Context
### Purpose
Turn a task into a bounded working set.

### Owns
- task normalization
- task classification
- anchor resolution
- neighborhood expansion for the task
- pruning
- snippet selection
- initial scope assembly

### Main Output
The main output of this layer is the task-context pack.

### Why This Layer Matters
This is the first layer where Aethyme becomes directly useful to agents.
Aethyme is not just a graph if it can emit a bounded navigation object for a task.

## Layer 4: Guidance
### Purpose
Shape how the agent should navigate and operate.

### Owns
- recommended reading order
- recommended expansion order
- out-of-scope explanation
- risk-aware warnings
- likely edit scope
- likely no-touch zones

### Outputs
- navigation guidance
- scope guidance
- review guidance

### Rules
This is advisory but real. It should materially affect how an agent proceeds.
It is not yet enforcement.

## Layer 5: Policy
### Purpose
Overlay human and organizational control on top of facts and guidance.

### Owns
- manually defined high-risk areas
- advisory restrictions
- escalation-required zones
- later agent-profile overlays
- later fine-grained permissions

### Outputs
- policy overlays attached to packs and guidance

### Rules
Policy must stay separate from repository facts.
It is an overlay, not a replacement for mapping or discoverability.

## Layer 6: Action Support
### Purpose
Help agents make bounded, reviewable changes.

### Owns
- edit-scope suggestions
- blast-radius analysis
- pre-change checks
- post-change impact pack
- review-pack generation
- later constrained autofix support

### Outputs
- suggested edit set
- expected impact
- review-oriented context

### Rules
Action support is downstream of understanding.
Aethyme should not optimize for autonomous action before it can reliably shape navigation and scope.

## Layer 7: Evaluation
### Purpose
Prove that Aethyme improves agent work.

### Owns
- benchmark repos
- benchmark tasks
- control prompt
- with-vs-without-Aethyme runs
- token metrics
- retry metrics
- review burden metrics
- time metrics

### Outputs
- evaluation reports
- regression signals
- product truth

### Rules
The benchmark protocol is part of the product, not an afterthought.
Without it, Aethyme becomes hard to validate and easy to overclaim.

## Layer 8: Agent Management
### Purpose
Run a deterministic management layer on top of Aethyme.

### Owns
- agent profiles
- policy-linked execution modes
- performance tracking per agent
- later governance and permissions

### Outputs
- managed agent configurations
- performance reports
- policy-aware execution modes

### Rules
This is a later layer. It depends on the earlier layers being real.

## Phased Implementation Model
Aethyme should be built in phases that follow the layers.

| Phase | Goal | Main layers |
|---|---|---|
| 1. Foundation Mapping | build a trustworthy substrate | 0, 1 |
| 2. Discoverability | make the substrate usable | 2 |
| 3. Context Packs v1 | emit bounded task packs | 3 |
| 4. Guidance v1 | shape agent navigation | 4 |
| 5. Evaluation Harness | prove product value | 7 |
| 6. Action Support | constrain code changes | 6 |
| 7. Policy v1 | add advisory control | 5 |
| 8. Managed Agent Layer | support deterministic agent management | 8 |

### Phase 1: Foundation Mapping
#### Goal
Build a trustworthy structural substrate.

#### Includes
- repo intake
- file and symbol graph
- deterministic edge model
- basic risk taxonomy
- confidence model

#### Exit Criteria
- repos can be indexed deterministically
- symbols, files, and edges are queryable
- high-risk path tagging works
- mapping quality is measurable

### Phase 2: Discoverability
#### Goal
Make the repository map usable as a navigation backend.

#### Includes
- symbol, file, and path lookup
- dependency frontier
- impact frontier
- anchor candidate ranking
- deterministic ordering

#### Exit Criteria
- the same query returns stable results
- dependency and impact queries are usable
- search quality is good enough to seed tasks

### Phase 3: Context Packs v1
#### Goal
Produce minimal, non-LLM task-context packs.

#### Includes
- task taxonomy
- task normalization
- anchor resolution
- pack schema
- pruning rules
- snippet selection

#### Exit Criteria
- one task in, one stable pack out
- packs are small enough to be useful
- the same task and repo state yield near-identical packs

### Phase 4: Guidance v1
#### Goal
Start shaping agent navigation behavior.

#### Includes
- navigation order
- in-scope list
- out-of-scope list
- high-risk warnings
- likely no-touch zones

#### Exit Criteria
- packs tell agents what to read first
- packs tell agents what to avoid
- risky areas are surfaced clearly

### Phase 5: Evaluation Harness
#### Goal
Measure whether Aethyme actually improves agent work.

#### Includes
- control prompt
- with-vs-without-Aethyme benchmark harness
- token measurement
- retry measurement
- review burden measurement
- repository explanation benchmark
- small code-change benchmark
- later CTF-style benchmark tasks

#### Exit Criteria
- repeatable benchmarks exist
- Aethyme can be compared against baseline
- token and retry improvements are observable

### Phase 6: Action Support
#### Goal
Support bounded code changes, not just navigation.

#### Includes
- edit-scope suggestions
- blast-radius hints
- post-change impact pack
- review pack
- safer autofix integration

#### Exit Criteria
- Aethyme helps constrain changes
- reviewability improves
- scope creep becomes measurable

### Phase 7: Policy v1
#### Goal
Add advisory control over repository areas.

#### Includes
- user-defined high-risk areas
- policy overlay model
- escalation-required annotations
- advisory blocked zones

#### Exit Criteria
- policy changes pack and guidance output
- manually defined high-risk areas work
- advisory controls are explainable

### Phase 8: Managed Agent Layer
#### Goal
Turn Aethyme into a deterministic agent control plane.

#### Includes
- agent profiles
- policy-linked execution modes
- performance management
- governance surface
- later enforceable permissions

#### Exit Criteria
- agents can run under defined navigation and policy modes
- performance can be compared per profile
- management is real, not conceptual

## First-Class Product Unit
The first-class external product unit is the task-context pack.

Internally, Aethyme works with:
- files
- symbols
- edges
- risk annotations
- confidence annotations

Externally, it should emit:
- a bounded task-context pack

The pack is the primary navigation object for an agent.
It should replace broad repository wandering with a smaller, deterministic working set.

## Context Pack v1
### Non-LLM First
The first version of the context pack should be fully deterministic.

Aethyme itself should emit the pack structure without requiring an LLM.
LLM-generated summaries may be added later, but they should not define pack membership, scope, or risk.

### Role
The context pack is:
- a bounded working set
- a navigation control surface
- a scope-shaping object
- an evidence bundle for the task

### Recommended Fields
- `task`
- `goal`
- `anchors`
- `summary`
- `snippets`
- `dependencies`
- `impact`
- `in_scope`
- `out_of_scope`
- `risk_flags`
- `navigation_order`
- `confidence`
- `budget`

### Output Style
Default output should include both:
- structured machine-readable content
- raw supporting evidence

The summary is the navigation layer.
The snippets are the proof layer.

## Task Model
The first task categories should be explicit.

Recommended v1 task kinds:
- `explain_repo`
- `explain_component`
- `change_symbol`
- `trace_impact`

The first product focus is single-task code changes.
Repository explanation is still important because it is a clean navigation benchmark.

## Task-To-Anchor Resolution
Aethyme should resolve anchors deterministically.

### Resolution Cascade
1. explicit mentions
2. keyword extraction
3. candidate lookup
4. anchor scoring
5. anchor selection

### Selection Rules
- prefer symbols over files when a symbol is reliable
- prefer files over folders when a file is specific enough
- prefer narrow anchors over broad central anchors
- include the reason each anchor was selected

## Scope Model
The first version should distinguish between different kinds of scope.

At minimum, Aethyme should reason about:
- likely relevant scope
- likely edit scope
- likely impact scope
- likely out-of-scope areas

This should not collapse into a single label.

## Out-Of-Scope Model
Out-of-scope should be treated as a real feature.

Out-of-scope can mean:
- not relevant to the task
- too far from the current dependency frontier
- high-risk unless explicitly needed
- low-confidence relation
- policy-restricted later

High-risk areas are especially important in this category.

## Risk Model
### Purpose
Risk exists to help the agent avoid costly or dangerous expansion.

### v1 Taxonomy
Recommended initial taxonomy:
- auth
- permissions
- secrets
- migrations
- infra
- billing
- shared-core
- destructive paths

### Detection Sources
Risk can come from:
- path rules
- naming rules
- graph position later
- user-defined overlays later

### Long-Term Direction
Users should eventually be able to define their own high-risk areas.
That capability belongs in the policy layer, not the mapping layer.

## Policy Direction
### Short Term
Policy is advisory first.
It should influence context packs and guidance, but not yet hard-block execution.

### Long Term
Policy should support:
- user-defined high-risk areas
- escalation-required zones
- blocked zones
- role or profile-based rights
- later file, folder, and function-level controls

## Evaluation Model
### Core Principle
Aethyme should be evaluated against a control condition.

The right question is not only whether a query looks good.
The real question is whether agents do better work with Aethyme than without it.

### Primary Metrics
Primary metrics are:
- token usage
- retry count
- human review burden

Secondary metrics include:
- wall-clock time
- number of files touched
- percent of changes outside expected scope

### First Benchmark
The first benchmark should be a structured repository explanation task.
This is a clean way to measure navigation quality, token usage, and time.

### Next Benchmark
The next benchmark should be a small, fixed code-change task.
That is closer to the actual product goal.

### Future Benchmark
A CTF-style benchmark suite is a strong long-term direction.
It provides fixed goals, measurable success, and harder-to-game evaluations.

## Language And Runtime Direction
Aethyme should move as much deterministic engine logic to Rust as is reasonable.
Python should remain the delivery and orchestration layer.

### Rust First
Rust should own, over time:
- mapping kernels
- discoverability kernels
- context-pack types and assembly
- scope and risk inference kernels
- graph expansion and pruning kernels
- later policy evaluation where determinism and performance matter

### Python Retained
Python should keep, for now:
- FastAPI delivery layer
- CLI layer
- auth enforcement layer
- scorecard orchestration
- SDKs
- migrations and operational wiring
- evaluation harnesses
- product orchestration around the Rust engine

## Recommended v1 Product Scope
The first meaningful Aethyme product should include only:
1. Repo Intake
2. Mapping
3. Discoverability
4. Context Packs v1
5. Guidance v1
6. Evaluation Harness

It should not yet try to fully deliver:
- policy enforcement
- broad automation
- managed agent platform features

## Near-Term Technical Priorities
1. define the Rust engine boundary clearly
2. implement mapping and discoverability kernels in Rust
3. implement task-context pack assembly in Rust
4. expose thin Python adapters over the Rust engine
5. build the evaluation harness in Python
6. improve graph quality and confidence labeling underneath the pack layer

## Near-Term Product Priorities
1. reduce irrelevant context for single-task code changes
2. produce stable task-context packs
3. surface high-risk and out-of-scope areas clearly
4. prove token and retry improvement against a control condition
5. keep the system honest and small while the substrate solidifies

## Long-Term Outcome
If successful, Aethyme Core becomes:
- the primary interface between AI coding agents and repositories
- a deterministic navigation substrate
- a scope and risk shaping system
- later, a policy-aware execution substrate
- later, a deterministic agent management and performance management layer
