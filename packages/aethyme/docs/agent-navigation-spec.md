# Aethyme Core: Agent Navigation Spec

Last Updated: 2026-03-08

## Purpose

This document is the canonical product and technical specification for Aethyme
Core. It defines:

- what Aethyme Core is trying to achieve
- the tiered architecture of the product
- the primary product objects and their data contracts
- the MCP surface agents interact with
- the ecosystem integration model
- the phased implementation order
- the runtime and language split

This document is intentionally comprehensive. Shorter operational docs should
point back to this one.

Context documents:

- [`vision.md`](vision.md) — the product vision and mental model
- [`architecture/research-informed-architecture-memo.md`](architecture/research-informed-architecture-memo.md) — research context for architectural decisions

## Executive Thesis

Aethyme Core is a GPS for AI coding agents navigating repositories.

Its job is to make repository work:

- more deterministic
- more context-efficient
- lower-retry
- easier to review
- increasingly scope-aware
- eventually permission-aware
- continuously improving from real execution data

The product is not a generic code search system and not a generic agent
platform. It is a live, learning navigation substrate for agent work on
codebases.

## Product Goal

The first real goal is to make single-task code changes meaningfully better.

Aethyme should help agents:

1. find the right starting point for their specific task type
2. gather the smallest useful context, shaped by task intent
3. understand dependency and impact boundaries
4. avoid irrelevant or high-risk areas unless needed
5. operate in a smaller, more reviewable scope
6. use the right tools for the task type
7. get better at all of the above over time, per repo

## What Aethyme Core Is

Aethyme Core is:

- a repository mapping engine (the road network)
- a lens system for task-typed navigation (the route mode)
- a live navigation session layer (the GPS runtime)
- a learning system that improves from execution data (traffic intelligence)
- later, a policy-aware execution substrate
- later, a foundation for deterministic agent management

## What Aethyme Core Is Not

Aethyme Core is not:

- a customer-facing identity product
- a generic SaaS shell
- a general-purpose observability platform
- a vague AI platform without a hard substrate
- a broad agent orchestration system in v1
- a static code index with no runtime intelligence

## Product Boundary

### `packages/aethyme`

Owns:

- repository intake and mapping
- lens definitions and lens-weighted traversal
- navigation session management
- enriched response assembly
- scope and risk inference
- tool scoping via registry integration
- telemetry ingestion from Chau7
- learned weight management
- evaluation and replay
- advisory guidance and later policy enforcement
- scorecard and repo quality signals
- local autofix/operator tooling
- auth enforcement and tenant isolation
- MCP server, API, and CLI adapters over the core engine

### `packages/aethyme-cloud`

Owns:

- identity issuance
- user, org, tenant, and admin lifecycle
- SaaS UX and management surfaces
- later agent management and policy UX
- later fleet-level performance reporting UI

## Canonical Model

The canonical business hierarchy is:

- Platform
- Org
- Tenant
- Repository
- Graph

The business hierarchy describes ownership and isolation.

## Tiered Architecture

Aethyme is structured as five tiers that interact in a feedback loop, not as a
strict sequential pipeline.

```
                    ┌───────────────────────┐
                    │   Tier 5: Control     │
                    │   Policy, Management  │
                    └───────────┬───────────┘
                                │ overlays on
                    ┌───────────▼───────────┐
                    │  Tier 2: Navigation   │◄──── learned weights
                    │  Lenses, Sessions,    │      from Tier 4
                    │  Enriched Responses   │
                    └───────────┬───────────┘
          reads from            │ queries against
┌─────────────────────┐        │
│  Tier 1: Substrate  │◄───────┘
│  Intake, Mapping,   │
│  Overlays           │
└─────────────────────┘
          ▲                     │ agent behavior
          │                     ▼
          │         ┌───────────────────────┐
          │         │  Tier 3: Ecosystem    │
          │         │  Chau7 telemetry,     │
          │         │  Registry, Aeptus     │
          │         └───────────┬───────────┘
          │                     │ signals
          │         ┌───────────▼───────────┐
          └─────────│  Tier 4: Intelligence │
                    │  Learning, Evaluation │
                    │  Replay               │
                    └───────────────────────┘
```

### Tier 1: Substrate

Build the map from the repository. Deterministic. Computed once per repo
snapshot.

| Layer | Goal | Main output | Owner |
|---|---|---|---|
| 1.0 Repo Intake | Normalize a repository | repo identity, snapshot, metadata | Python |
| 1.1 Structural Mapping | Build file and area graph | files, directories, areas, containment | Rust |
| 1.2 Semantic Mapping | Build code entity graph | symbols, edges, imports, calls, references | Rust |
| 1.3 Documentation & Config | Make docs and config first-class | doc nodes, config nodes, document/configure edges | Rust |
| 1.4 Overlays | Attach risk, navigation hints, and inferred signals | risk annotations, entrypoint hints, navigation overlay | Rust |
| 1.5 Normalization & Persistence | Stabilize and store the graph | normalized repository graph | Rust |

### Tier 2: Navigation

The GPS runtime. Processes agent queries through task-typed lenses, tracks
trajectory, assembles enriched responses. This is where the product becomes
live and contextual.

| Layer | Goal | Main output | Owner |
|---|---|---|---|
| 2.0 Lenses | Offer task-typed navigation profiles | lens catalog, lens selection, graph reweighting | Rust |
| 2.1 Navigation Session | Maintain live traversal state per agent task | session state, trajectory, position awareness | Rust + Python |
| 2.2 Enriched Responses | Assemble contextually shaped responses | enriched neighborhoods, scope signals, risk annotations, tool suggestions | Rust |

### Tier 3: Ecosystem

Integration with external systems. Bridges Aethyme to the observation layer
(Chau7), the tool supply chain (Registry), and the governance layer (Aeptus).

| Layer | Goal | Main output | Owner |
|---|---|---|---|
| 3.0 Telemetry Ingestion | Receive live agent behavior from Chau7 | event stream: files read, tools used, tokens, outcomes | Python |
| 3.1 Tool Scoping | Integrate with Registry for skill/MCP availability | tool recommendations per lens, trust scores | Python |
| 3.2 Posture Signals | Emit behavioral and risk signals to Aeptus | agent posture data: access, scope compliance, audit trail | Python |

### Tier 4: Intelligence

The learning flywheel. Turns execution data into map improvements. Provides
evaluation and replay for continuous product validation.

| Layer | Goal | Main output | Owner |
|---|---|---|---|
| 4.0 Learning | Update map weights from execution telemetry | node signals, edge signals, area signals, lens calibration | Rust + Python |
| 4.1 Evaluation | Measure product value continuously | token/retry/review metrics, regression signals | Python |
| 4.2 Replay | Re-run tasks with isolated variables | model grading, map regression, tool evaluation | Python |

### Tier 5: Control

Organizational rules overlaid on the navigation system. Starts advisory,
matures into enforceable controls.

| Layer | Goal | Main output | Owner |
|---|---|---|---|
| 5.0 Policy | Apply advisory and later enforceable rules | risk overlays, escalation hints, blocked zones | Python |
| 5.1 Agent Management | Control agent behavior at scale | profiles, permissions, routing, fleet performance | Cloud later |

### Why Tiers Instead Of Layers

The previous architecture was a strict sequential pipeline: build the graph,
then query it, then add guidance, then add policy, then evaluate.

The GPS model requires a feedback loop. Navigation is not downstream of mapping
and upstream of evaluation. Navigation, ecosystem integration, learning, and
evaluation form a cycle. Tiers separate concerns without imposing false
sequencing.

The four concerns that must not collapse:

- repository facts (Tier 1)
- live navigation and task shaping (Tier 2)
- ecosystem integration and learning (Tiers 3 + 4)
- organizational control (Tier 5)

## Lenses

Lenses are named, described navigation profiles that reweight the graph based
on task type. They are a first-class product object, not a hidden parameter.

### How Lenses Work

Aethyme offers available lenses with descriptions. The agent picks one. The
choice is logged and reviewable.

Each lens defines:

- **Edge weights**: which edge types are bright (high weight), dim (low weight),
  or invisible (zero weight)
- **Node promotion**: which node types are promoted or suppressed in results
- **Traversal strategy**: depth-first, breadth-first, or impact-radiating
- **Depth limits**: how far to expand in each direction
- **Risk behavior**: when risk overrides the default weights (e.g., promote docs
  in high-risk areas even if the lens normally suppresses docs)
- **Tool profile**: which registry skills are relevant and promoted

### Default Lens Catalog

```
debug
  edges:    calls bright, references bright, tests promoted
  nodes:    functions bright, error handlers promoted, docs dim unless risk high
  strategy: depth-first along call chains
  tools:    debugger, log-analyzer, test-runner promoted

feature
  edges:    contains bright, documents bright, similar-pattern promoted
  nodes:    architecture docs bright, configs promoted, deep internals dim
  strategy: breadth-first, area-hopping
  tools:    scaffolding, test-runner promoted

refactor
  edges:    imports bright, calls bright, references bright, inherits bright
  nodes:    type boundaries promoted, tests promoted
  strategy: impact-radiating from anchors
  tools:    test-runner required, impact-checker promoted

security
  edges:    calls bright, reads/writes bright, configures bright
  nodes:    risk zones bright, auth paths promoted, everything else heavily dimmed
  strategy: depth-first along sensitive paths
  tools:    vuln-scanner promoted, secrets-detector promoted

explain
  edges:    contains bright, documents bright, configures bright
  nodes:    docs bright, structure bright, code details dim
  strategy: breadth-first, overview-first
  tools:    minimal
```

### Lens Data Model

```ts
type Lens = {
  id: string
  name: string
  description: string
  edge_weights: Record<string, number>     // edge kind -> weight [0.0, 1.0]
  node_weights: Record<string, number>     // node kind -> weight [0.0, 1.0]
  traversal: "depth_first" | "breadth_first" | "impact_radiating"
  max_depth: number
  risk_override: boolean                   // if true, high-risk areas promote
                                           // docs/warnings regardless of weights
  tool_profile: {
    promoted: string[]                     // registry skill IDs
    required: string[]                     // must be available
    suppressed: string[]                   // hidden from recommendations
  }
}
```

### Lens Calibration

Lenses have designed defaults (static) and learned adjustments (dynamic).

The learned adjustments come from execution telemetry. If the `debug` lens on
a specific repo consistently sees agents succeed when they read documentation,
the learned adjustment gradually boosts doc weights for that lens on that repo.

```ts
type LensCalibration = {
  lens_id: string
  repo_id: string
  edge_adjustments: Record<string, number>   // additive delta
  node_adjustments: Record<string, number>   // additive delta
  updated_at: string
  sample_count: number                       // how many tasks informed this
  decay_factor: number                       // recent data weighted higher
}
```

### Lens Switching And Composition

Agents may switch lenses mid-session. The switch is logged and the navigation
session adjusts trajectory tracking.

Limited lens composition is supported: an agent can request a primary lens with
a modifier (e.g., `feature + security`). This applies the primary lens weights
with the modifier's risk overrides and tool profile merged in.

## Navigation Sessions

A navigation session is the GPS runtime for a single agent task.

### Session Lifecycle

1. Agent connects and starts a session with a task description
2. Aethyme proposes lenses; agent selects one
3. Agent queries the graph through the session; Aethyme returns enriched
   responses shaped by lens, trajectory, and learned weights
4. Chau7 streams live telemetry; Aethyme tracks trajectory and adjusts
5. Task completes; session closes; telemetry flows to learning layer

### Session State

```ts
type NavigationSession = {
  session_id: string
  repo_id: string
  snapshot_id: string
  task_description: string
  active_lens: string
  lens_history: { lens_id: string; switched_at: string }[]
  trajectory: {
    node_id: string
    timestamp: string
    source: "agent_query" | "chau7_observation"
  }[]
  tokens_consumed: number
  tools_used: string[]
  status: "active" | "completed" | "abandoned"
}
```

### Trajectory Tracking

As Chau7 streams events (file reads, tool calls), Aethyme updates the session
trajectory. This trajectory affects subsequent responses:

- If the agent is on track (traversing the expected subgraph for this lens and
  task), responses continue normally
- If the agent drifts (enters an area with low relevance to the task),
  subsequent responses silently promote paths back toward the relevant subgraph
- If the agent repeatedly visits a known dead-end area, that area is dimmed
  further in subsequent responses

The steering is always embedded in the response. There is no separate
intervention channel.

## Enriched Responses

Enriched responses are the primary product output. When an agent queries
Aethyme, it does not get a raw file or a flat list. It gets a contextually
shaped neighborhood.

### What Shapes A Response

1. **The active lens** — edge weights determine what's included and promoted
2. **The session trajectory** — where the agent has been affects what's next
3. **The risk profile** — high-risk areas trigger additional context (docs,
   warnings) even if the lens would normally suppress them
4. **Learned weights** — historical execution data adjusts node/edge prominence

### Response Depth Levels

- **Silent shaping** — weights shape which edges the agent sees at all. The
  agent never knows certain paths were suppressed.
- **Enriched** — contextually important extras are attached without being asked.
  Agent queries `auth.py`, gets `auth.py` + `oauth_client.py` + architecture
  doc because the lens + risk profile says so.
- **Annotated** — risk flags, cost signals, scope notes included inline in
  the response metadata.
- **Redirected** — the query target is not the best starting point. Response
  includes the requested content but prominently suggests a better anchor.

### Enriched Response Schema

```ts
type EnrichedResponse = {
  session_id: string
  query: {
    target: string                         // what the agent asked for
    lens: string                           // active lens
  }
  primary: {
    node: GraphNode
    snippet?: string
    edges: GraphEdge[]
  }
  enrichments: {
    node: GraphNode
    reason: string                         // why this was included
    source: "lens" | "risk" | "trajectory" | "learned"
    snippet?: string
  }[]
  scope: {
    in_scope: string[]                     // node IDs likely relevant
    out_of_scope: string[]                 // node IDs to avoid
    risk_flags: {
      node_id: string
      risk: string
      level: string
      reason: string
    }[]
  }
  annotations: {
    kind: string
    message: string
    confidence: number
  }[]
  suggested_next: string[]                 // node IDs to explore next
  tool_suggestions: {
    skill_id: string
    reason: string
    trust_score: number
  }[]
}
```

## MCP Surface

Aethyme exposes its navigation capabilities as an MCP server. This is the
primary interface agents use to interact with Aethyme.

### Core Tools

#### `aethyme_list_lenses`

Returns available lenses with descriptions. The agent reads these and picks one.

```ts
// Request
{ repo_path: string }

// Response
{
  lenses: {
    id: string
    name: string
    description: string
    tool_profile: { promoted: string[]; required: string[] }
  }[]
}
```

#### `aethyme_start_session`

Starts a navigation session for a task. Returns the session ID and initial
orientation (repo overview shaped by the selected lens).

```ts
// Request
{
  repo_path: string
  task: string
  lens: string
}

// Response
{
  session_id: string
  overview: EnrichedResponse       // repo-level orientation through the lens
  anchors: {                       // suggested starting points for this task
    node_id: string
    reason: string
    confidence: number
  }[]
}
```

#### `aethyme_navigate`

The primary navigation tool. The agent says where it wants to go; Aethyme
returns an enriched response shaped by the lens, trajectory, and learned
weights.

```ts
// Request
{
  session_id: string
  target: string                   // node ID, file path, or symbol name
}

// Response: EnrichedResponse
```

#### `aethyme_expand`

Expand the neighborhood of a node in a specific direction.

```ts
// Request
{
  session_id: string
  node_id: string
  direction: "callers" | "callees" | "dependencies" | "dependents"
           | "impact" | "docs" | "tests" | "config"
  depth?: number
}

// Response: EnrichedResponse
```

#### `aethyme_scope`

Ask what is in scope and out of scope for the current task, given the
trajectory so far.

```ts
// Request
{
  session_id: string
}

// Response
{
  in_scope: { node_id: string; reason: string; confidence: number }[]
  out_of_scope: { node_id: string; reason: string; confidence: number }[]
  risk_zones: { node_id: string; risk: string; level: string }[]
  suggested_tools: { skill_id: string; reason: string }[]
}
```

#### `aethyme_switch_lens`

Switch the active lens mid-session.

```ts
// Request
{
  session_id: string
  lens: string
  modifier?: string                // optional secondary lens for composition
}

// Response
{
  previous_lens: string
  new_lens: string
  reorientation: EnrichedResponse  // the current position re-rendered through
                                   // the new lens
}
```

#### `aethyme_end_session`

End the navigation session. Triggers telemetry flush to the learning layer.

```ts
// Request
{
  session_id: string
  outcome?: "success" | "failure" | "abandoned"
}

// Response
{
  session_summary: {
    duration_ms: number
    nodes_visited: number
    tokens_consumed: number
    lens_switches: number
    drift_events: number
  }
}
```

### Stateless Tools (No Session Required)

These tools work without a session for quick, one-off queries.

#### `aethyme_overview`

Get a repo overview without starting a session. Uses the `explain` lens by
default.

```ts
// Request
{ repo_path: string; lens?: string }
// Response: EnrichedResponse (repo-level)
```

#### `aethyme_lookup`

Look up a specific symbol, file, or path.

```ts
// Request
{ repo_path: string; query: string }
// Response
{
  matches: {
    node: GraphNode
    score: number
    context: string
  }[]
}
```

## Canonical Repograph Architecture

The repograph is the substrate beneath every higher tier. It should not be
treated as "search index plus a few edges."

The target is a layered, multi-resolution, function-centric repository graph
with overlays for navigation, risk, policy, and performance.

### Core Principle

The graph should be:

- function-centric for code semantics
- file and area-centric for navigation
- documentation and config aware for explanation
- overlay-friendly for risk, policy, and task shaping

The repograph must answer:

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

The repograph is modeled in distinct layers:

| Graph layer | Meaning | Examples |
|---|---|---|
| Structural truth | Hard repository facts | repo, area, directory, file, contains |
| Semantic truth | Code and runtime facts | function, class, imports, calls, references |
| Inferred meaning | Useful but probabilistic structure | entrypoints, representative areas, doc links |
| Control overlays | Human and system constraints | high-risk zones, policy rules, escalation |
| Learned overlays | Execution-derived signals | anchor quality, dead-end signals, cost signals |

These layers must stay separate. Policy, task scope, and learned signals are
not graph truth.

### Canonical Node Taxonomy

#### Structural nodes

- `repo`, `snapshot`, `workspace`, `area`, `directory`, `file`

#### Code semantic nodes

- `module`, `namespace`, `class`, `interface`, `trait`, `struct`, `enum`
- `function`, `method`, `field`, `constant`, `type_alias`, `macro`, `test_case`

#### Documentation nodes

- `doc`, `doc_section`, `decision_record`, `runbook`, `spec`, `example`

#### Config and runtime nodes

- `config`, `manifest`, `dependency`, `service`, `runtime_target`
- `build_target`, `pipeline`, `environment`

#### Asset and content nodes

- `asset`, `dataset`, `schema`, `template`, `content_object`

### Canonical Edge Taxonomy

#### Structural edges

- `contains`, `belongs_to`, `part_of`

#### Code semantic edges

- `defines`, `declares`, `imports`, `exports`, `references`, `calls`
- `reads`, `writes`, `mutates`, `constructs`, `returns`, `raises`
- `inherits`, `implements`, `specializes`, `tests`, `mocks`

#### Documentation and config edges

- `documents`, `describes`, `configures`, `builds`, `deploys`
- `depends_on`, `uses`, `generates`, `transforms`

#### Navigation and scope edges

- `anchors`, `in_scope_for`, `out_of_scope_for`, `entrypoint_for`
- `starting_point_for`, `relevant_to`, `irrelevant_to`

#### Control edges

- `high_risk_for`, `restricted_for`, `blocked_for`
- `requires_escalation_for`, `allowed_for`

### Canonical Graph Unit

The smallest meaningful code node should be `function` or `method`.

The main navigation backbone: `repo -> area -> file -> function`

- function-level for change reasoning
- file and area-level for navigation, scope, and control

### Canonical Graph Record Shape

```ts
type GraphNode = {
  id: string
  kind: string
  label: string
  layer: "truth" | "semantic" | "inferred" | "control" | "learned"
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

type GraphEdge = {
  id: string
  from: string
  to: string
  kind: string
  layer: "truth" | "semantic" | "inferred" | "control" | "learned"
  snapshot_id: string
  confidence: number
  source: string
  metadata: Record<string, unknown>
}

type GraphAnnotation = {
  id: string
  target_id: string
  kind: string
  layer: "control" | "learned"
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

### Confidence Model

- `1.0` structural fact
- `0.95` compiler or indexer-resolved semantic fact
- `0.9` parser-derived semantic fact
- `0.75` strong inference
- `0.5` weak inference
- `0.25` advisory heuristic only

Higher tiers must be able to distinguish truth from inference.

## Substrate Extraction Passes

The repograph is built through explicit passes. Each pass consumes one explicit
artifact and emits one explicit artifact.

### Pass 0. Repo Intake

Creates the stable root context: repo id, snapshot id, canonical root, build
settings.

Output artifact: `RepoContext`

### Pass 1. Filesystem Structure Extraction

Builds structural coverage: `repo`, `area` candidates, `directory`, `file`,
and `contains` edges.

Output artifact: `StructureGraph`

### Pass 2. File Classification

Classifies each file by role: source, test, doc, config, manifest, build,
asset, generated, binary, cache. Determines deep-parsing eligibility.

Output artifact: `ClassifiedFiles`

### Pass 3. Area Formation

Creates stable repo zones for navigation. Assigns files and directories to
top-level areas and later inferred subsystem areas.

Output artifact: `AreaMap`

### Pass 4. Code Parsing

Runs language-specific parsers against supported source files. Extracts
functions, methods, classes, imports, and raw code relations.

Output artifact: `ParsedCodeUnits`

### Pass 5. Symbol Normalization

Normalizes parser output into deterministic graph entities: stable function
nodes, class nodes, `defines` edges, normalized imports.

Output artifact: `NormalizedCodeGraph`

### Pass 6. Code Relationship Resolution

Resolves semantic relations: `calls`, `references`, `inherits`, `implements`,
and `entrypoint_for` candidates.

Output artifact: `ResolvedCodeRelations`

### Pass 7. Documentation Extraction

Makes documentation first-class: `doc` nodes, `doc_section` nodes,
`documents`, `belongs_to`, and `references` edges.

Output artifact: `DocumentationGraph`

### Pass 8. Config / Manifest Extraction

Makes manifests and config files first-class: `config` and `manifest` nodes,
`configures`, `depends_on`, and `entrypoint_for` edges.

Output artifact: `ConfigGraph`

### Pass 9. Asset / Content Registration

Registers non-code repo material as graph entities.

Output artifact: `AssetGraph`

### Pass 10. Risk Annotation

Attaches high-risk overlays from deterministic rules: auth, permissions,
migrations, infra, secrets, billing, shared-core, destructive paths.

Output artifact: `RiskOverlay`

### Pass 11. Entry Point And Navigation Inference

Infers entrypoints, starting points, and representative entities.

Output artifact: `NavigationOverlay`

### Pass 12. Graph Normalization

Sorts, deduplicates, validates, and stabilizes the graph.

Output artifact: `NormalizedRepositoryGraph`

### Pass 13. Persistence

Stores normalized nodes, edges, and annotations.

### Pass 14. Learned Weight Integration

Merges any existing learned weights from previous sessions into the persisted
graph as learned-layer annotations. On a fresh repo with no history, this pass
is a no-op.

Output artifact: `WeightedRepositoryGraph`

## Pass Data Contracts

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

### `ClassifiedFiles`

```ts
type ClassifiedFile = {
  file_id: string
  path: string
  language?: string
  role: "source" | "test" | "doc" | "config" | "manifest"
      | "build" | "asset" | "generated" | "binary" | "cache"
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

### `RiskOverlay`

```ts
type RiskOverlay = {
  repo_context: RepoContext
  annotations: {
    target_id: string
    risk: "auth" | "permissions" | "migrations" | "infra"
        | "secrets" | "billing" | "shared-core" | "destructive"
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

## Learning Model

### What Is Learned

Learning produces weight adjustments on nodes, edges, and areas. These are
stored as learned-layer annotations, separate from structural truth.

#### Node Signals

```ts
type NodeSignal = {
  node_id: string
  signal: "anchor_quality" | "token_cost" | "danger" | "dead_end"
  lens_id?: string                  // signal may be lens-specific
  value: number                     // [0.0, 1.0] or absolute (tokens)
  sample_count: number
  last_updated: string
}
```

- `anchor_quality` — how often this node is a productive starting point for a
  given lens/task type
- `token_cost` — average tokens consumed when this node enters context
- `danger` — how often changes to this node correlate with CI failure or review
  rejection
- `dead_end` — how often agents visit this node and it turns out irrelevant

#### Edge Signals

```ts
type EdgeSignal = {
  edge_id: string
  signal: "traversal_success" | "traversal_cost" | "shortcut"
  lens_id?: string
  value: number
  sample_count: number
  last_updated: string
}
```

- `traversal_success` — how often following this edge leads to task success
- `traversal_cost` — average token overhead of following this edge
- `shortcut` — this edge skips a commonly taken multi-hop path

#### Area Signals

```ts
type AreaSignal = {
  area_id: string
  signal: "drift_zone" | "hotspot" | "stable_zone"
  value: number
  sample_count: number
  last_updated: string
}
```

- `drift_zone` — agents enter this area frequently but it is rarely part of
  successful changes
- `hotspot` — high change frequency with high outcome variance
- `stable_zone` — rarely touched, changes usually succeed

### Signal Decay

Signals decay over time. The codebase changes, so old signals lose relevance.
Decay is applied using a half-life model: a signal's effective weight is
reduced by 50% after a configurable number of days (default: 30).

### Signal Sources

All learning signals come from Chau7 telemetry:

- file reads → node visit tracking
- tool calls → tool effectiveness tracking
- task outcomes (success, failure, abandoned) → traversal success scoring
- token counts → cost signal updates
- CI results (observed via Chau7 command blocks) → danger signal updates
- git diff (observed via Chau7 git detection) → edit scope tracking

Aethyme does not require agent cooperation to learn. Chau7 observes
transparently.

## Evaluation And Replay

### Continuous Evaluation

Every real task is an evaluation. The navigation session captures:

- tokens consumed
- files read vs files changed (noise ratio)
- scope drift (nodes visited outside the scope boundary)
- lens appropriateness (did the lens match the actual traversal pattern)
- time to completion
- outcome

These metrics are computed automatically at session close.

### Replay Engine

Because sessions are deterministic (repo snapshot + map version + lens + tool
set), tasks can be replayed with isolated variables:

| Variable | What it tests |
|---|---|
| Model | Same map, same tools. Which model navigates best? |
| Map version | Same model. Did learned weights improve outcomes? |
| Tool set | Same model, same map. Did a skill change matter? |
| No Aethyme vs Aethyme | The fundamental product value proof |

Replay runs pin the repo at the original commit, use the specified map version,
and provide the same MCP tools. The only variable that changes is the one being
tested.

### Eval Integrity — No Overfitting

Evals are diagnostics. They must never become optimization targets.

The engine, pipeline, skills, and all agent-facing surfaces must remain
task-type-agnostic. It is forbidden to add code paths that target specific eval
scenarios, inject task-type-specific data into generic pipelines, or tune
heuristics based on eval score regressions. If an eval reveals a weakness, the
fix must be generic — something that improves the system for all tasks on all
repositories, not a tweak that raises a specific metric.

The test: "Would I make this change if the eval didn't exist?" If no, the
change is overfitting and must not ship.

See [`guides/eval-protocol.md`](../guides/eval-protocol.md) for the full rule
with detailed examples.

### Nightly Evaluation

For enterprise deployments, a nightly job can:

1. Select representative tasks from the day's sessions
2. Replay each task against multiple models and map versions
3. Produce a report: token comparison, success rates, drift metrics, model
   grading

## Ecosystem Integration

### Chau7 Integration

Chau7 is the sensor network. It provides:

- **Live telemetry stream**: file reads, tool calls, tokens, command outcomes,
  git changes, streamed to Aethyme in real time via event stream
- **Session capture**: full session recordings for replay
- **Agent detection**: which agent (Claude Code, Codex, Gemini CLI, etc.) is
  running, providing agent-type-specific behavior tracking

Aethyme does not require any agent modification. Chau7 observes agent behavior
transparently through its terminal integration.

### Registry Integration

The skills/MCP registry is the tool supply chain. It provides:

- **Skill catalog**: available skills with security audits and trust scores
- **Tool recommendations**: per-lens tool suggestions based on trust and
  effectiveness data
- **Effectiveness tracking**: aggregated data on which tools work for which
  task types, fed back from Chau7 telemetry across the user base

Aethyme lenses reference registry skill IDs in their tool profiles. When an
agent queries Aethyme, tool suggestions from the registry are included in
enriched responses.

### Aeptus Integration

Aeptus is the governance layer for organizational security posture. Aethyme
feeds it:

- **Agent access records**: which agents accessed which repos and areas
- **Scope compliance**: did the agent stay within expected boundaries
- **Risk exposure**: did the agent enter high-risk zones, and was it warranted
- **Audit trail**: full session trajectory for compliance evidence
- **Tool usage**: which skills/MCPs were used, from which registry sources

This data powers the agent posture module within Aeptus.

## Task Model

### Task Kinds

- `explain_repo` — repository explanation and orientation
- `explain_component` — component or area understanding
- `change_symbol` — single-task code change
- `trace_impact` — impact analysis for a proposed change
- `debug` — find and fix a defect
- `refactor` — restructure without behavior change
- `security_review` — assess security posture of an area

The first product focus is `change_symbol` (single-task code changes).
`explain_repo` is the clean navigation benchmark.

### Task-To-Anchor Resolution

Aethyme resolves anchors deterministically.

Resolution cascade:

1. explicit mentions in the task description
2. keyword extraction
3. candidate lookup against the graph
4. anchor scoring (relevance, specificity, lens alignment)
5. anchor selection

Selection rules:

- prefer symbols over files when a symbol is reliable
- prefer files over folders when a file is specific enough
- prefer narrow anchors over broad central anchors
- include the reason each anchor was selected

## Scope Model

Aethyme distinguishes between:

- **likely relevant scope** — nodes the agent should probably read
- **likely edit scope** — nodes the agent might change
- **likely impact scope** — nodes affected by changes to the edit scope
- **likely out-of-scope** — nodes the agent should avoid

Out-of-scope reasons:

- not relevant to the task
- too far from the dependency frontier
- high-risk unless explicitly needed
- low-confidence relation
- policy-restricted

## Risk Model

### v1 Taxonomy

- auth, permissions, secrets, migrations, infra, billing, shared-core,
  destructive paths

### Detection Sources

Risk comes from:

- path and naming rules (static, Tier 1)
- graph position (inferred, Tier 1)
- learned danger signals (dynamic, Tier 4)
- user-defined overlays (policy, Tier 5)

### Risk Behavior In Navigation

When an agent navigates into a high-risk area, the enriched response overrides
normal lens behavior:

- documentation for the area is promoted regardless of lens weights
- risk annotations are included inline
- out-of-scope signals are more aggressive
- tool suggestions may include security-related skills

## Policy Direction

### Short Term

Policy is advisory. It influences enriched responses and scope signals but does
not hard-block agent execution.

### Long Term

Policy should support:

- user-defined high-risk areas
- escalation-required zones
- blocked zones
- role or profile-based rights
- file, folder, and function-level controls
- lens restrictions (certain lenses may be required or forbidden for certain
  areas)

## Language And Runtime Direction

### Rust Owns

- substrate extraction passes (mapping, parsing, normalization)
- graph storage and traversal
- lens-weighted graph queries
- enriched response assembly
- scope and risk inference
- learned weight storage and merge
- context pack assembly

### Python Owns

- MCP server
- API delivery layer
- CLI
- auth enforcement
- telemetry ingestion from Chau7
- learning signal computation
- evaluation harness and replay engine
- registry and Aeptus integration
- scorecard orchestration
- SDKs

## Phased Implementation

| Phase | Goal | Tiers |
|---|---|---|
| 1. Substrate | Deterministic repo map | Tier 1 |
| 2. Lenses & MCP | Task-typed navigation via MCP | Tier 2 (lenses + MCP) |
| 3. Sessions | Live trajectory tracking | Tier 2 (sessions + enrichment) |
| 4. Telemetry Bridge | Chau7 event stream ingestion | Tier 3.0 |
| 5. Learning v1 | Execution data → weight updates | Tier 4.0 |
| 6. Evaluation & Replay | Model grading, map regression | Tier 4.1 + 4.2 |
| 7. Tool Scoping | Registry integration in lenses | Tier 3.1 |
| 8. Policy v1 | Advisory controls | Tier 5.0 |
| 9. Posture Signals | Aeptus integration | Tier 3.2 |
| 10. Agent Management | Profiles, permissions, fleet | Tier 5.1 |

### Phase 1: Substrate

Build a trustworthy structural map.

Exit criteria:

- repos can be indexed deterministically
- symbols, files, edges, and areas are queryable
- risk tagging works from static rules
- confidence model is applied throughout

### Phase 2: Lenses & MCP

Expose navigation through lenses via MCP.

Exit criteria:

- default lens catalog is defined and usable
- agents can query `aethyme_list_lenses`, `aethyme_start_session`,
  `aethyme_navigate`, `aethyme_expand`, `aethyme_scope`
- same query through different lenses produces visibly different results
- lens choice is logged

### Phase 3: Sessions

Track agent trajectory in real time.

Exit criteria:

- navigation sessions maintain state across queries
- trajectory tracking influences subsequent responses
- drift detection produces measurable steering in responses

### Phase 4: Telemetry Bridge

Ingest live agent behavior from Chau7.

Exit criteria:

- Chau7 event stream connects to Aethyme
- file reads, tool calls, tokens, and outcomes are captured
- session trajectory is updated from Chau7 observations

### Phase 5: Learning v1

Turn execution data into map improvements.

Exit criteria:

- node signals (anchor quality, dead-end, danger) are computed from telemetry
- edge signals (traversal success, cost) are computed
- lens calibration adjustments are applied per repo
- learned weights visibly change navigation behavior

### Phase 6: Evaluation & Replay

Prove product value and enable model grading.

Exit criteria:

- replay engine can re-run a session with a different model or map version
- token/time/drift metrics are computed per replay
- nightly evaluation job produces comparison reports

### Phase 7: Tool Scoping

Integrate the registry into lens-based navigation.

Exit criteria:

- lenses include tool profiles referencing registry skill IDs
- enriched responses include tool suggestions with trust scores
- tool effectiveness data flows back to the registry

### Phase 8: Policy v1

Add advisory organizational controls.

Exit criteria:

- user-defined high-risk areas modify enriched responses
- policy overlays are explainable and separate from structural truth

### Phase 9: Posture Signals

Feed agent behavior data to Aeptus.

Exit criteria:

- agent access records and scope compliance signals flow to Aeptus
- audit trails are complete and queryable

### Phase 10: Agent Management

Deterministic agent control at scale.

Exit criteria:

- agent profiles constrain lens selection and tool availability
- performance can be compared per profile
- management is real, not conceptual

## Long-Term Outcome

If successful, Aethyme Core becomes:

- the default repository navigation layer for agents
- a live, learning GPS that improves with every task
- the substrate for deterministic agent execution
- the base for agent performance management
- the base for permission-aware AI change systems
- part of a four-product ecosystem providing full observability, governance,
  and tool supply chain management for AI agent work
