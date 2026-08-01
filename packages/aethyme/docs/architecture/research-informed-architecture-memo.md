# Research-Informed Architecture Memo

Last Updated: 2026-03-06

## Purpose
This memo turns the recent repository-graph and agent-navigation literature into direct architecture guidance for Aethyme Core.

It is not a paper summary for its own sake. It exists to answer:
- what the research supports strongly
- what Aethyme should adopt
- what Aethyme should reject
- what remains an open bet

This memo complements, but does not replace:
- [`../agent-navigation-spec.md`](../agent-navigation-spec.md)
- [`core-architecture.md`](core-architecture.md)
- [`rust-transition.md`](rust-transition.md)

## Paper Set
This memo is based on the following papers and resources:

- CodexGraph: https://arxiv.org/abs/2408.03910
- LocAgent: https://arxiv.org/abs/2503.09089
- CoSIL: https://arxiv.org/abs/2503.22424
- RANGER: https://arxiv.org/abs/2509.25257
- Knowledge Graph Based Repository-Level Code Generation: https://arxiv.org/abs/2505.14394
- The Navigation Paradox in Large-Context Agentic Coding / CodeCompass: https://arxiv.org/abs/2602.20048
- Reliable Graph-RAG for Codebases: https://arxiv.org/abs/2601.08773
- AgentArmor: https://arxiv.org/abs/2508.01249
- Agint: https://arxiv.org/abs/2511.19635
- Awesome-GraphRAG: https://github.com/DEEP-PolyU/Awesome-GraphRAG
- Awesome-Repo-Level-Code-Generation: https://github.com/YerbaPage/Awesome-Repo-Level-Code-Generation

## Executive Position
The literature supports a clear direction:

1. Aethyme should be graph-first, not vector-first.
2. The graph should be function-centric for code semantics.
3. The graph should also be multi-resolution:
   - repo
   - area
   - directory
   - file
   - class
   - function
   - doc
   - config
4. Graph extraction should be deterministic and parser-derived.
5. Aethyme should emit task-shaped graph slices, not raw graph dumps.
6. Navigation quality matters more than raw context-window size.
7. Later control, policy, and performance layers are justified, but they should sit on top of graph truth rather than inside it.

## Core Research Conclusions

### 1. Function-level graphing is not optional
LocAgent and CoSIL both validate function-level reasoning as a meaningful working unit for localization and issue resolution.

Implication for Aethyme:
- function and method nodes should be first-class
- call, reference, and containment relations should be central
- task packs should prefer function anchors when available

### 2. Function-level is necessary but not sufficient
The literature does not support a function-only graph. Repo-level navigation still needs:
- file structure
- subsystem or area structure
- documentation
- configuration and manifests
- entrypoint hints

Implication for Aethyme:
- the graph must be multi-resolution
- area and file nodes remain essential for scope, navigation, and policy

### 3. Deterministic graph extraction is the correct foundation
Reliable Graph-RAG strongly supports deterministic AST-derived graphs over LLM-mediated graph extraction for correctness-sensitive tasks.

Implication for Aethyme:
- graph truth should come from deterministic parsing and normalization
- LLMs should not define the graph
- LLMs may sit above the graph for later summarization or ranking, but not inside graph construction

### 4. Large context does not solve navigation
The Navigation Paradox paper is the strongest conceptual validation of the Aethyme thesis. More tokens do not remove the need for structured navigation. They change the failure mode from missing context to weak salience and bad attention allocation.

Implication for Aethyme:
- the core product is not "bigger context"
- the core product is guided navigation and bounded context
- Aethyme should become the primary navigation layer, not an optional helper

### 5. Persistent graphs are more aligned with the product than fully dynamic graph construction
CoSIL shows dynamic graph construction can work for issue localization, but LocAgent, CodexGraph, and RANGER all support the value of repository-level graph persistence.

Implication for Aethyme:
- build and persist the repository graph
- derive task-specific slices dynamically from that persisted graph
- do not rely on reconstructing the graph from scratch during each task

### 6. Hybrid retrieval is useful, but graph should remain authoritative
RANGER and the repository-level KG generation paper show the value of combining symbolic structure with semantic retrieval.

Implication for Aethyme:
- exact and graph-driven retrieval should remain primary
- semantic or vector retrieval can help candidate generation later
- vector search should not define scope, impact, or policy

### 7. Control and behavior layers are justified, but they come later
AgentArmor and CodeCompass support the idea that agent behavior can be shaped by graph abstractions, not just code retrieval.

Implication for Aethyme:
- risk and policy overlays are not speculative overreach
- performance and management layers are legitimate long-term layers
- they should be added after graph truth and navigation are strong

## Decisions To Adopt

### Adopt: deterministic parser-derived graph construction
Aethyme should:
- parse repository structure deterministically
- derive code semantics from AST or language tooling
- normalize graph entities and edges with stable IDs
- attach confidence to inferred relations

### Adopt: function-centric code graph
The smallest meaningful code node should be:
- function
- method

With supporting nodes:
- class
- file
- area
- repo

### Adopt: multi-resolution repository operating graph
The final graph should include:
- structure
- code semantics
- docs
- configs
- navigation overlays
- risk overlays
- later policy and performance overlays

### Adopt: task-shaped graph slices
Aethyme should not expose raw graph traversal as the only product.
It should derive:
- anchors
- dependency frontier
- impact frontier
- in-scope
- out-of-scope
- risk hints
- navigation order

That slice is the context-pack basis.

### Adopt: graph truth first, policy later
Risk, restriction, permission, and escalation should be modeled as overlays on graph entities.

They should not be mixed into graph truth extraction.

### Adopt: evaluation against a baseline
The literature and the product thesis both require actual performance comparison.

Aethyme should be judged by:
- lower token usage
- fewer retries
- lower review burden
- better scope discipline

Against:
- a no-Aethyme baseline
- same task
- same repo
- same agent or model

## Decisions To Reject

### Reject: LLM-generated graph truth
Aethyme should not rely on an LLM to decide:
- what entities exist
- what edges exist
- what files were actually indexed

That creates correctness, reproducibility, and coverage problems.

### Reject: vector-first repository retrieval
Embeddings can help later with recall.
They should not define:
- graph truth
- scope
- impact
- permission
- no-touch areas

### Reject: file-only repographing
A file-only graph is too coarse for:
- change localization
- impact analysis
- scope control
- minimal task-context packs

### Reject: function-only repographing
A pure function graph loses:
- repo structure
- subsystem boundaries
- docs
- configs
- folder and area risk zones

### Reject: raw graph dumps as the product surface
The literature supports graph-based systems, but raw graph access alone is not the product.

Aethyme should return:
- structured navigation objects
- bounded slices
- task-shaped outputs

### Reject: assuming bigger context windows remove the need for navigation
This is exactly the failure mode the Navigation Paradox paper challenges.

## Open Research Bets

### Open bet: exact right balance between persistent graph and dynamic task-time expansion
Aethyme should keep a persistent graph.
What remains open is how much dynamic expansion happens during task resolution versus how much is precomputed.

### Open bet: best task-to-anchor resolution strategy
The literature supports graph-guided localization, but not a single definitive pipeline for every task class.

Aethyme still needs to determine:
- how much exact matching to use
- how much graph traversal to use
- how much later semantic ranking to add

### Open bet: when vector retrieval becomes worth adding
The research suggests a hybrid future, but not that vectors are required on day one.

Aethyme should defer this until:
- graph coverage is strong
- graph-derived packs are stable
- baseline navigation performance is measured

### Open bet: best interface for graph interaction
CodexGraph uses Cypher.
LocAgent uses explicit graph tools.
RANGER combines Cypher and graph exploration.

Aethyme should not commit too early to:
- raw query language
- opaque tool-only access
- one monolithic pack shape

The right interface may be:
- internal graph API
- deterministic context-pack emitter
- limited expert query surface later

### Open bet: how much of the higher-level guidance should be model-free
The graph itself should be model-free and deterministic.
What remains open is how much of:
- task normalization
- prioritization
- narrative rendering
- pack explanation
stays deterministic versus using an LLM later.

## Direct Architecture Consequences For Aethyme

### Graph extraction
Aethyme should implement a staged pipeline:
1. repo intake
2. filesystem extraction
3. file classification
4. area formation
5. language-native code parsing
6. symbol normalization
7. code relationship resolution
8. documentation extraction
9. config extraction
10. risk and navigation inference
11. graph normalization and persistence

### Graph schema
Aethyme should treat the following as first-class:
- repo
- area
- directory
- file
- class
- function
- doc
- config

And the following relations as central:
- contains
- belongs_to
- defines
- imports
- calls
- references
- documents
- configures
- entrypoint_for

### Product surface
The first real product object should not be "graph query results".
It should be a graph-derived task slice:
- context pack
- scope boundary
- risk boundary
- navigation order

### Control model
Risk and policy should be overlays:
- high-risk zones
- no-touch zones
- escalation-required zones
- later agent-profile permissions

### Performance model
Later, Aethyme should ingest execution traces and benchmark runs as overlays rather than mixing them with graph truth.

## Immediate Implications For Current Core

### What current Aethyme is directionally right about
- graph-first architecture
- Rust as the deterministic engine direction
- local-first proof path
- context packs as a first-class object

### What current Aethyme still lacks
- richer graph coverage for docs/config/assets
- stronger function-level call and reference extraction
- true multi-resolution graph quality on mixed repos
- clearer task-derived graph slicing
- stronger evaluation against real agent runs

## Concrete Implementation Delta

This section translates the research conclusions into a direct upgrade plan for the current repograph.

### Current design
The current Rust engine is still structurally thin.

Main active pieces today (paths post-2026-05-08 module split):
- [`../../rust/crates/aethyme-engine/src/repo.rs`](../../rust/crates/aethyme-engine/src/repo.rs)
- [`../../rust/crates/aethyme-engine/src/map.rs`](../../rust/crates/aethyme-engine/src/map.rs)
- [`../../rust/crates/aethyme-engine/src/graph/search.rs`](../../rust/crates/aethyme-engine/src/graph/search.rs)
- [`../../rust/crates/aethyme-engine/src/graph/neighborhood.rs`](../../rust/crates/aethyme-engine/src/graph/neighborhood.rs)
- [`../../rust/crates/aethyme-engine/src/graph/anchors.rs`](../../rust/crates/aethyme-engine/src/graph/anchors.rs)
- [`../../rust/crates/aethyme-engine/src/pipeline.rs`](../../rust/crates/aethyme-engine/src/pipeline.rs)
- [`../../rust/crates/aethyme-engine/src/model/symbol.rs`](../../rust/crates/aethyme-engine/src/model/symbol.rs)
- [`../../rust/crates/aethyme-engine/src/model/edge.rs`](../../rust/crates/aethyme-engine/src/model/edge.rs)

Current properties:
- broad filesystem scan
- basic language detection
- partial symbol extraction
- import-heavy edge extraction
- path-based risk tagging
- task-pack derivation built mostly from lightweight heuristics

Current limitations:
- no first-class `area` nodes in the engine model
- no first-class `doc` nodes
- no first-class `config` nodes
- weak function-level relationship resolution
- `RepositoryMap` still collapses multiple extraction concerns into one output artifact
- `ExplainRepo` is still derived from anchors more than from graph structure

### Target design
The target repograph should be:
- function-centric for semantic reasoning
- multi-resolution for navigation and control
- deterministic and parser-derived
- built in explicit passes
- queryable as graph truth
- sliced into task-shaped navigation objects later

The minimum serious target ontology is:
- `repo`
- `area`
- `directory`
- `file`
- `class`
- `function`
- `doc`
- `config`

With central relations:
- `contains`
- `belongs_to`
- `defines`
- `imports`
- `calls`
- `references`
- `documents`
- `configures`
- `entrypoint_for`

### Current-to-target delta

| Concern | Current state | Target state |
|---|---|---|
| Structure | repo + files only in practice | repo + area + directory + file |
| Code nodes | partial symbol list | class + function + method graph |
| Code edges | mostly imports | defines + imports + calls + references |
| Docs | implicit at best | first-class doc nodes and links |
| Configs | implicit at best | first-class config nodes and links |
| Risk | path rules in one map pass | overlay on graph entities |
| Extraction | loose combined pass | explicit pass artifacts |
| Task packs | heuristic pack building | graph-slice derivation |

## Module Plan

### Rust modules to add or elevate first

These should become the real repograph core:

- `area.rs`
  - area node model
  - top-level and inferred area contracts

- `file.rs`
  - normalized file node model
  - role/language/generated metadata

- `class.rs`
  - class/type node model

- `function.rs`
  - function/method node model

- `doc.rs`
  - documentation node model

- `config.rs`
  - manifest/config/build node model

- `graph.rs`
  - normalized graph node/edge/annotation container

- `passes/`
  - one module per extraction pass

### Existing Rust modules to rewrite

- [`repo.rs`](../../rust/crates/aethyme-engine/src/repo.rs)
  - keep for intake and snapshot discovery only
  - remove semantic responsibilities from it

- [`map.rs`](../../rust/crates/aethyme-engine/src/map.rs)
  - split into explicit pass outputs
  - stop treating the repository map as one flattened artifact

- [`model/symbol.rs`](../../rust/crates/aethyme-engine/src/model/symbol.rs)
  - replace the generic symbol bucket with class/function-focused node types
  - keep generic symbol support only if needed for unresolved references later

- [`model/edge.rs`](../../rust/crates/aethyme-engine/src/model/edge.rs)
  - expand edge taxonomy
  - store edge source and numeric confidence, not just a coarse enum

- [`graph/search.rs`](../../rust/crates/aethyme-engine/src/graph/search.rs)
  - switch from symbol/file lookup over a flat map to lookup over the normalized graph

- [`graph/neighborhood.rs`](../../rust/crates/aethyme-engine/src/graph/neighborhood.rs)
  - expand from shallow dependency neighbors to typed graph neighborhoods

- [`graph/anchors.rs`](../../rust/crates/aethyme-engine/src/graph/anchors.rs)
  - derive anchors from graph views rather than file-name heuristics only

- [`pipeline.rs`](../../rust/crates/aethyme-engine/src/pipeline.rs)
  - consume graph slices and overlays rather than the current thin map

### Python modules to keep thin (all removed — retirement Phase 6)

This section was advice for a hybrid system: keep the Python layer thin
so graph truth stays in the engine. The python-retirement finished the
argument by deleting the layer. `src/` is gone as of 2026-08-01 and
`packages/aethyme` has no Python product code at all.

- `src/indexing/engine.py` (removed 2026-08-01 — a build-if-stale helper
  for the dev test harness by the end; the transport adapter it once was
  died with the native router in Phase 1)
- `src/indexing/repository_snapshot.py`, `src/contracts/`, `src/models/`,
  `src/cli.py` (removed 2026-08-01, retirement Phase 6)
- `src/eval/` orchestration (removed 2026-07-13 with the eval harness; see [`eval-mining-notes.md`](eval-mining-notes.md))
- `src/rendering/context_pack.py` (removed 2026-07-28 — renderers went native in `task_cli.rs`, retirement Phase 1)

The successor discipline is the same rule stated of Rust crates: the
engine owns graph truth, and the CLI/enhance crates stay orchestration.

## Pass-Oriented Refactor Plan

### Pass 1: structure
Create artifacts for:
- repo
- area
- directory
- file

Primary files:
- `repo.rs`
- `area.rs`
- `file.rs`
- `passes/structure.rs`

### Pass 2: classification
Create a file classification pass:
- source
- test
- doc
- config
- asset
- generated

Primary files:
- `file.rs`
- `passes/classify.rs`

### Pass 3: code parsing
Create language-specific parsing outputs for:
- class
- function
- method
- imports
- basic references

Primary files:
- `class.rs`
- `function.rs`
- `indexer/python.rs`
- `indexer/typescript.rs`
- later `indexer/rust.rs`
- `passes/code_parse.rs`

### Pass 4: symbol normalization
Resolve deterministic IDs and ownership:
- file defines class
- file defines function
- class defines method
- file or symbol belongs_to area

Primary files:
- `graph.rs`
- `passes/normalize.rs`

### Pass 5: code relationship resolution
Resolve:
- calls
- references
- imports
- inheritance

Primary files:
- `edge.rs`
- `passes/relations.rs`

### Pass 6: docs and config extraction
Promote docs and configs to first-class nodes.

Primary files:
- `doc.rs`
- `config.rs`
- `passes/docs.rs`
- `passes/config.rs`

### Pass 7: overlays
Attach:
- risk
- navigation hints
- later policy

Primary files:
- `risk.rs`
- `guidance.rs`
- later `policy.rs`
- `passes/overlays.rs`

### Pass 8: graph views and slices
Derive:
- repo overview view
- area overview view
- function neighborhood view
- task-context slices

Primary files:
- `search.rs`
- `neighborhood.rs`
- `anchors.rs`
- `pipeline.rs`

## First Rewrite Priorities

The highest-value rewrite order is:

1. split `RepositoryMap` into pass outputs
2. replace generic symbol modeling with class/function models
3. add area/doc/config as first-class graph entities
4. expand edge taxonomy and confidence model
5. rebuild `ExplainRepo` on graph views instead of ad hoc anchors

## Practical Working Rule

If a feature cannot be expressed as:
- graph truth
- inferred overlay
- task slice

then it should not be added to the repograph yet.

## Recommended Near-Term Priorities
1. Strengthen repograph coverage before chasing more benchmark surface area.
2. Make function-level extraction reliable for priority languages first.
3. Add docs and config extraction as first-class graph layers.
4. Improve explain-repo by deriving it from graph structure, not just anchor lists.
5. Keep evaluation honest: compare with and without Aethyme on the same tasks.

## Working Rule
If a proposed feature weakens deterministic graph truth in exchange for convenience, Aethyme should reject it unless the gain is measurable and the truth layer remains authoritative.

## Practical Summary
The literature does not suggest that Aethyme should become a generic RAG layer or a generic agent tool router.

It suggests something sharper:
- build a deterministic, function-centric, multi-resolution repository operating graph
- derive task-shaped graph slices from it
- use those slices to reduce context waste, retries, and review burden
- later add risk, policy, and performance overlays on top of that graph
