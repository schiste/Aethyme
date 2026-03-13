# Graphability And Navigability Signals

Last Updated: 2026-03-07

## Purpose

These signals measure how easy a repository is for Aethyme to:

1. map deterministically
2. navigate efficiently
3. constrain task scope correctly
4. support agent work with low wasted motion

They are not generic code-quality scores.
They are repository-structure signals for graph construction, navigation quality, and later agent-readiness scoring.

## Use In The Stack

These signals should be used in four places:

1. `repograph extraction`
   - file classification
   - area profiling
   - config/doc linking
   - parser coverage reporting
2. `navigation and ranking`
   - anchor ranking
   - overview ranking
   - scope derivation
   - out-of-scope selection
3. `reports and diagnostics`
   - repo inspection
   - explain-repo reference outputs
   - benchmark interpretation
4. `scorecard later`
   - repo legibility
   - agent navigability
   - control-readiness

## Signal Set

### 1. Boundary Clarity

What it measures:
- whether the repo has coherent areas with understandable ownership

Concrete heuristics:
- cross-area import ratio
- cross-area call ratio
- fraction of files with one dominant area assignment
- count of broad dumping-ground paths such as `utils`, `helpers`, `common`, `misc`
- doc/config links that stay inside the same area

Good signal:
- most code relations stay inside the owning area
- top areas have distinct file and symbol clusters

Bad signal:
- many relations cross areas without a clear boundary
- many files sit in generic common buckets

Where it lives:
- repograph:
  - area profiling
  - file-to-area assignment confidence
- navigation:
  - overview area ranking
  - task scope narrowing
- scorecard later:
  - `boundary_clarity`

### 2. Entrypoint Clarity

What it measures:
- whether runtime, build, or execution entrypoints are explicit and easy to locate

Concrete heuristics:
- count of `entrypoint_for` edges with direct code targets
- number of configs/manifests with clear entrypoint linkage
- number of ambiguous entrypoint candidates per area
- presence of explicit startup files (`main`, `lib`, app bootstraps, CLI roots)

Good signal:
- each major area has one or a few clear entrypoint paths

Bad signal:
- many ambiguous manifests
- startup implied only by convention or side effects

Where it lives:
- repograph:
  - config extraction
  - entrypoint inference
- navigation:
  - config ranking
  - explain-repo outputs
  - ownership tasks
- scorecard later:
  - `entrypoint_clarity`

### 3. Naming Clarity

What it measures:
- whether names help localization instead of obscuring it

Concrete heuristics:
- ratio of generic names (`utils`, `helpers`, `common`, `temp`, `misc`)
- token overlap between task anchors and file/function names
- number of duplicated generic filenames across areas
- ratio of functions/files with descriptive names above a minimum token length

Good signal:
- names expose responsibility and area identity

Bad signal:
- many generic names
- repeated ambiguous filenames across the repo

Where it lives:
- repograph:
  - file and symbol metadata
- navigation:
  - anchor resolution
  - search ranking
- scorecard later:
  - `naming_clarity`

### 4. Config Hygiene

What it measures:
- whether configs are predictable, distinct by role, and easy to connect to behavior

Concrete heuristics:
- count of manifest/project/runtime/build configs by area
- count of duplicate config families in one area
- ratio of configs with explicit code or area links
- ratio of noisy/generated/autosave JSON files misclassified as configs

Good signal:
- configs are role-distinct and linked to areas or entrypoints

Bad signal:
- many overlapping configs
- content/state blobs mixed with operational config

Where it lives:
- repograph:
  - config classification
  - config role families
- navigation:
  - key config ranking
  - ownership tasks
- scorecard later:
  - `config_hygiene`

### 5. Documentation Attachment

What it measures:
- whether docs are attached to real areas, files, and behavior

Concrete heuristics:
- fraction of docs linked to an area
- fraction of docs linked to a file/config
- ratio of architecture docs versus detached notes
- coverage of top areas by overview docs

Good signal:
- each important area has attached docs
- overview docs match actual repo structure

Bad signal:
- many docs float without structural links
- docs cluster in one folder with weak code/config connection

Where it lives:
- repograph:
  - doc extraction
  - doc-to-area/file/config linking
- navigation:
  - explain-repo
  - graph docs
- scorecard later:
  - `documentation_attachment`

### 6. Hidden Coupling

What it measures:
- how much behavior depends on implicit relationships that are hard to see statically

Concrete heuristics:
- ratio of low-confidence versus high-confidence semantic edges
- number of runtime/config entrypoints without explicit code linkage
- count of side-effect-heavy imports or registration files
- spread between file imports and resolved function-call coverage

Good signal:
- most important relations are explicit and parseable

Bad signal:
- many important behaviors depend on reflection, side effects, or weakly linked config

Where it lives:
- repograph:
  - edge confidence
  - unresolved linkage diagnostics
- navigation:
  - confidence notes
  - impact and scope warnings
- scorecard later:
  - `hidden_coupling`

### 7. Edit Locality

What it measures:
- whether small tasks stay in a bounded file and area set

Concrete heuristics:
- average number of files returned by change-task scope
- caller/callee expansion spread by area
- percentage of change-task scopes that stay in one area
- symbol-to-file projection width for change tasks

Good signal:
- small tasks stay inside a compact file set

Bad signal:
- even simple tasks immediately widen to many areas or files

Where it lives:
- navigation:
  - task scope
  - task next
  - change-task ranking
- scorecard later:
  - `edit_locality`

### 8. Structural Redundancy

What it measures:
- how much duplicate or near-duplicate structure confuses navigation

Concrete heuristics:
- count of duplicate config families per area
- count of repeated wrapper files with weak semantic value
- duplicate generic filenames across areas
- repeated overview documents with overlapping scope

Good signal:
- one clear config or doc per role

Bad signal:
- duplicated project files
- many near-identical wrappers and aliases

Where it lives:
- repograph:
  - config/doc/file family grouping
- navigation:
  - overview deduplication
  - ranking penalties
- scorecard later:
  - `structural_redundancy`

### 9. Parser Visibility

What it measures:
- how much of the repo can be mapped with deterministic semantics

Concrete heuristics:
- parseable source file ratio
- unsupported language ratio
- binary/generated file ratio
- fraction of high-confidence edges versus total inferred edges

Good signal:
- most important repo areas are parseable

Bad signal:
- important areas are opaque, generated, or unsupported

Where it lives:
- repograph:
  - file classification
  - parser coverage reporting
- navigation:
  - confidence weighting
  - overview warnings
- scorecard later:
  - `parser_visibility`

### 10. Navigation Yield

What it measures:
- whether graph-backed navigation actually gives compact, useful working sets

Concrete heuristics:
- anchor precision on benchmark tasks
- average in-scope file count by task kind
- out-of-scope precision
- percentage of `task next` items later retained in the final pack
- benchmark correctness per token spent

Good signal:
- top anchors are correct and scope is small

Bad signal:
- navigation repeatedly returns noisy or irrelevant items

Where it lives:
- navigation:
  - task anchors
  - task scope
  - task next
  - eval interpretation
- scorecard later:
  - `navigation_yield`

## First Implementation Priority

The first five signals should be implemented or exposed explicitly first:

1. boundary clarity
2. entrypoint clarity
3. config hygiene
4. hidden coupling
5. parser visibility

Reason:
- they improve both the repograph and task navigation immediately
- they are measurable from current graph artifacts
- they do not require external run telemetry

## Implementation Placement

### Repograph

Add explicit metrics or annotations during extraction for:
- boundary clarity
- entrypoint clarity
- config hygiene
- documentation attachment
- parser visibility

These belong in:
- structure pass
- code pass
- docs pass
- configs pass
- overlay/annotation layer

### Navigation

Use these signals for:
- area ranking
- config ranking
- anchor confidence
- scope penalties
- out-of-scope selection
- change-task narrowing

### Scorecard

Do not mix these directly into the current scorecard yet.
First expose them as graph and navigation diagnostics.
Once stable across multiple repo types, promote them into scorecard dimensions.

## Reporting

These signals should eventually appear in:
- `repo inspect`
- `graph overview`
- explain-repo benchmark reports
- navigation benchmark reports

Each report should make clear:
- which signals were strong
- which were weak
- whether the repo was easy or hard to navigate structurally

## Rule

Do not turn these into vague summary labels.
Each signal must remain tied to concrete measurable heuristics and visible graph artifacts.
