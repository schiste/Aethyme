# Research Lessons Revised After Implementation

Last Updated: 2026-03-07

## Purpose

This memo updates the earlier research-informed architecture position after actual repograph and navigation implementation work in Aethyme Core.

The original research conclusions were directionally correct, but implementation exposed where the emphasis needed to change.

This document answers:

- what the research still supports strongly
- what implementation validated
- what implementation forced us to reframe
- what remains provisional and should be validated across more repositories

This memo complements:

- [`research-informed-architecture-memo.md`](research-informed-architecture-memo.md)
- [`../agent-navigation-spec.md`](../agent-navigation-spec.md)
- [`core-architecture.md`](core-architecture.md)

## What Still Holds

These decisions remain correct and should not be revisited:

1. Aethyme should be graph-first, not vector-first.
2. Graph extraction should be deterministic and parser-derived.
3. Code semantics should be function-centric.
4. The graph should be multi-resolution:
   - repo
   - area
   - directory
   - file
   - class
   - function
   - doc
   - config
5. Aethyme should derive task-shaped slices from the graph rather than dumping raw graph structure into prompts.

Implementation validated all five.

## What Implementation Changed

### 1. The product is not the graph. The product is graph-mediated navigation.

The original research reading emphasized:

- graph structure
- graph retrieval
- graph slices
- context packs

Implementation showed that this is incomplete.

The graph is the substrate.
The actual product is:

- anchor resolution
- scope derivation
- bounded expansion
- out-of-scope control
- task-specific navigation order

So the primary product object is no longer best described as "the graph" or even "the pack".

It is:

**the navigation layer built on top of the graph**

### 2. Task taxonomy is not optional.

The implementation work showed that:

- `ExplainRepo`
- config ownership tasks
- change tasks

all need different:

- anchors
- scope rules
- expansion logic
- output shapes

So task typing is no longer a useful design idea.
It is a required primitive.

### 3. Docs and configs matter more than the research summary originally emphasized.

The literature strongly emphasized code graphs.
Implementation on mixed repositories showed that:

- configs can be more important than code edges for some tasks
- docs are necessary for repo explanation
- area and ownership tasks often depend on config and doc links more than call graphs

So Aethyme should treat:

- docs
- configs
- manifests
- runtime descriptors

as first-class graph material, not auxiliary metadata.

### 4. Function-level semantics are necessary, but edit work still resolves at file level.

Implementation confirmed:

- functions are the correct semantic reasoning unit
- files remain the correct practical edit boundary
- areas remain the correct navigation and control boundary

So the right working model is:

- function-level for reasoning
- file-level for edits
- area-level for navigation and control

### 5. Persistent graph plus dynamic task-time slicing is the right combination.

The earlier memo favored persistent graphing over dynamic graph construction.
That remains correct.

Implementation adds a refinement:

- the graph should remain persistent
- actual usage should be strongly task-time and slice-driven

So the right design is:

- persistent graph for truth
- dynamic graph slicing for navigation

### 6. `ExplainRepo` is useful, but it is not the strongest benchmark.

Implementation exposed a weakness in using `ExplainRepo` as the primary proof:

- it can drift toward summarization heuristics
- it does not pressure function-level semantics enough
- it does not expose multi-hop navigation failures as clearly as targeted tasks

So benchmark priority should now be:

1. directed navigation tasks
2. change-task pre-navigation
3. repo explanation

## What We Should Reframe

### Reframe: graph retrieval -> navigation protocol

Earlier framing:

- retrieve from graph
- emit pack

Current better framing:

- classify task
- resolve anchors
- derive scope
- expand graph as needed
- emit bounded navigation artifacts

This is more accurate and more aligned with the actual product.

### Reframe: context packs -> one output of the navigation layer

Context packs still matter.
But they are no longer the entire story.

They are one output of:

- graph navigation
- scope shaping
- task-type-specific logic

### Reframe: repo understanding -> orientation benchmark

`ExplainRepo` should remain in the suite, but as:

- orientation benchmark
- overview quality benchmark

not as the main proof of repograph quality.

## What Is Still Provisional

The architecture is general enough.
Some ranking and selection behavior is not yet proven general.

The main provisional areas are:

1. area ranking
2. key config ranking
3. representative file selection
4. task keyword heuristics

These are currently heuristic and should be validated across multiple repo types.

## Current Working Position

After implementation, the strongest current statement is:

**Aethyme Core is a graph-mediated navigation system for repositories.**

It uses a deterministic multi-resolution repograph as the substrate, and its product value comes from:

- task-specific anchoring
- scoped navigation
- graph expansion
- bounded task slices
- later control and performance overlays

## Revised Priority Order

### Keep as primary investments

1. deterministic repograph quality
2. graph-mediated navigation
3. task taxonomy
4. scope and out-of-scope control
5. docs/config/runtime linkage

### Move down

1. raw graph-query expressiveness
2. repo explanation as the main benchmark
3. vector retrieval

## Practical Consequences For Aethyme

1. Continue improving the repograph, but always in service of navigation.
2. Treat task-specific navigation views as first-class product surfaces.
3. Evaluate using directed navigation and change-task benchmarks, not repo explanation alone.
4. Keep runner/eval integration separate from graph semantics.
5. Validate ranking behavior across multiple repository archetypes before treating it as general.

## Final Position

The research was right about the need for graphs.
Implementation clarified that the real product is not "graph retrieval".

The real product is:

**graph-mediated navigation and scope control for software tasks**

That is the revised architecture position Aethyme should build around.
