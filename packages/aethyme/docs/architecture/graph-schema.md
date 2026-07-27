# Aethyme Graph Schema

Last Updated: 2026-07-17

Status: **Implemented V1 durable graph contract.** The committed
source-of-truth graph lives under `.aethyme/graph/` as deterministic
fragments and index shards. The engine redb file at
`.aethyme/graph_store.redb` is a derived local query artifact, rebuilt from
those fragments by `aethyme-engine-cli index --repo <repo>`.

No schema versioning means every fragment-shape decision here is treated as
forever; push back hard before changing any field or enum that has landed in
the storage crates.

## 1. Principles

These rules apply uniformly across the schema. They were each decided
explicitly during the design conversation; if a future change appears to
violate one of them, that's the trip wire.

| Principle | Practical meaning |
|---|---|
| **No redundant display fields** | The ID is opaque (content hash). Human-readable forms are derived at render time from canonical fields (`kind`, `path`, `name`, `language`). Never store a `display_name` or `display_label`. |
| **No schema versioning** | Schema is forever. New facts get new kinds, never relaxed fields on existing kinds. |
| **Strong typing per kind** | Each node kind declares its own required fields. No catch-all attribute bag on canonical fields. |
| **Required-per-kind, never permissive optional** | When a fact is genuinely unavailable, emit a different kind (e.g. `UnresolvedSymbol`) rather than relax the field. |
| **Index time is one-time; query time must be fast** | Materialize derived facts at index time. The query path never recomputes what the indexer could have stored. |
| **Fragments are the source of truth** | Per-file fragments, overlay fragments, and per-module index shards live under `.aethyme/graph/` in git. The current engine redb graph store lives at `.aethyme/graph_store.redb`; it is derived from fragments and regenerated locally. |
| **Determinism is paramount** | Same source → byte-identical fragments on every machine, OS, parser version. Non-determinism is a bug, not a quirk. |
| **Neutral surface, language extensions** | Node kinds and edge kinds are language-neutral. Language-specific data lives under `extensions.<lang>` namespaces. |

## 2. Identity

### 2.1 Node ID shape

```
<kind>:<repo>:<symbol_content_hash>
```

- `<kind>` is the canonical kind (see §3). Lowercase, snake_case.
- `<repo>` is the repo namespace. Multi-repo systems use the workspace
  configuration to map a node's repo to a stable repo ID.
- `<symbol_content_hash>` is a 128-bit BLAKE3 hash, base32-encoded
  (lowercase, no padding, 26 chars), over the length-prefixed tuple:
  ```
  (repo, file_path, symbol_name, kind_name)
  ```
  Body content is deliberately NOT in the hash inputs. Including
  body would break the "edit-unrelated-symbol" stability row in
  §2.2 (every file edit would invalidate every symbol ID in the
  file). The ID identifies "this symbol at this path with this
  name and kind in this repo"; body content is a separate
  attribute on the node, not part of identity.

Example: `fn:aethyme:k3l2m9p4n8q6r2s7t1u5v3w8x9y0z1a2b3c4`

### 2.2 What ID stability covers

| Operation | ID survives? |
|---|---|
| Edit a function body that doesn't change its name | ✅ |
| Edit an unrelated function in the same file | ✅ |
| Move a function to a different file (rename) | ❌ |
| Rename a function in place | ❌ |
| Reformat (whitespace-only changes) | ✅ if normalized before hashing |

Symbol-rename tracking is intentionally out of scope.

### 2.3 Display forms (derived, not stored)

| Context | Computed from |
|---|---|
| Log line | `${kind}:${path}::${name}` |
| Agent-facing label | `${path}::${name}` |
| Compact list entry | `${name}` |
| Cross-repo display | `${repo}/${path}::${name}` |

These are pure projections of canonical fields. Renderers compute them;
the schema does not store them.

## 3. Node kinds

Every node kind declares: `kind` enum tag, required fields, optional
fields, `extensions: { <lang>: {...} }` namespace policy.

### 3.1 Container kinds

| Kind | Required fields | Notes |
|---|---|---|
| `repository` | `name`, `root_path`, `vcs` | Top of the namespace. |
| `directory` | `path` | One per source directory. |
| `module` | `path`, `name`, `language` | Logical module — a package, a namespace, a TS module. Different from `directory`. |
| `file` | `path`, `language`, `byte_size`, `content_hash` | Source code files. |
| `non_code_file` | `path`, `format` (`markdown`/`yaml`/`json`/`toml`/...) | Non-code first-class nodes (§3.5). |

### 3.2 Callable kinds (share the `Callable` protocol)

The `Callable` protocol is the common shape:
```
{ name, signature, parameters[], return_type, start_line, end_line, visibility }
```

All callable kinds must satisfy it.

| Kind | Adds | Notes |
|---|---|---|
| `function` | `is_top_level: bool` | Top-level or nested function. |
| `method` | `receiver_type`, `is_static`, `is_virtual` | Receiver type is the enclosing class/struct/trait. |
| `lambda` | `enclosing_callable_id`, `assigned_name?` | First-class node always; `assigned_name` is set when the lambda is assigned to a named variable (e.g. `const handler = () => …`). |

`Callable.signature` is a string; structured access is via `parameters[]`
and `return_type`.

### 3.3 Type-defining kinds

| Kind | Required | Notes |
|---|---|---|
| `class` | `name`, `start_line`, `end_line`, `visibility` | Python/JS/PHP classes; C#/Java classes. |
| `struct` | `name`, `start_line`, `end_line`, `visibility` | Rust/C/Go structs; not Python's `dataclass` (those are `class` with an `extensions.python.dataclass = true`). |
| `interface` | `name`, `start_line`, `end_line`, `visibility` | TS interfaces; Go interfaces. |
| `trait` | `name`, `start_line`, `end_line`, `visibility` | Rust traits. Closest neutral analogue for Java/C# interfaces; we keep `interface` and `trait` distinct because semantics differ on default methods. |
| `enum` | `name`, `variants[]`, `start_line`, `end_line`, `visibility` | Variants are inline; not a separate kind. |
| `type_alias` | `name`, `target_type`, `start_line`, `end_line`, `visibility` | TS `type =`, Rust `type =`. |

### 3.4 Sub-symbol kinds

| Kind | Required | Notes |
|---|---|---|
| `field` | `name`, `type`, `enclosing_type_id`, `visibility` | Class/struct fields. |
| `global_variable` | `name`, `type?`, `start_line`, `end_line` | Module-level bindings. |
| `parameter` | `name`, `type?`, `position`, `enclosing_callable_id` | First-class because dataflow analysis (future) needs them. |
| `statement` | `start_line`, `end_line`, `kind_tag` (e.g. `assign`/`if`/`return`) | Top-level AST statements within a function body. Tier 2 materialization. |
| `expression` | `start_line`, `end_line`, `kind_tag` | Deepest first-class node. Tier 3 materialization. |

Statement and expression nodes exist *in schema* (expression-level floor)
but are not materialized to fragments by default. Tier-2 expansion runs
on demand; tier-3 expansion runs when an intent explicitly asks for
expression-level evidence.

### 3.5 Non-code kinds

This is the layer that supports the documentation-as-graph vision.

| Kind | Required | Notes |
|---|---|---|
| `non_code_file` | `path`, `format` | The file itself. |
| `doc_section` | `enclosing_non_code_file_id`, `heading`, `start_line`, `end_line` | A heading-bounded section of a markdown doc. |
| `docstring` | `target_symbol_id`, `start_line`, `end_line`, `text_hash` | Attached to a callable/class/module via a `Documents` edge. |
| `comment` | `target_statement_id?`, `start_line`, `tag` (`todo`/`fixme`/`note`/`decision`/`see_also`/`other`), `text_hash` | Inline comments tagged with `other` are stored as attributes on the enclosing statement; only tagged comments become first-class nodes. |
| `config_value` | `enclosing_non_code_file_id`, `path` (e.g. `database.url`), `value_hash`, `start_line`, `end_line` | One per addressable config key. Lets `Configures` edges connect a value to the code that reads it. |

### 3.6 Partial-knowledge kind

| Kind | Required | Notes |
|---|---|---|
| `unresolved_symbol` | `name`, `expected_kind?`, `referenced_from_id` | Used when we see a reference (call, import) to a symbol we cannot locate. Prevents relaxing required fields on real kinds. |

## 4. Edge kinds

Edges carry their own attributes. Storage is one-way forward; a reverse
index is maintained per kind. Multi-edges (same `(src, dst, kind)`
appearing multiple times in source) collapse into one edge with a
`sites[]` attribute (§5.3).

### 4.1 Structural edges

| Kind | Direction | Common attributes | Notes |
|---|---|---|---|
| `contains` | parent → child | — | `repository` → `directory` → `file` → `class/function/...`. |
| `defines` | scope → symbol | — | A `function` that defines a nested `function`. |
| `imports` | importer → imported | `import_path`, `is_namespace`, `is_default`, `is_named` | A `file` or `module` imports another. |

### 4.2 Behavioral edges

| Kind | Direction | Common attributes | Notes |
|---|---|---|---|
| `calls` | caller → callee | `sites: [{line, is_in_branch, is_in_loop, kind_tag}, ...]` | Caller-owns; dangling allowed. |
| `inherits` | subclass → superclass | — | Single-inheritance languages; multi-inheritance produces multiple `inherits` edges. |
| `implements` | class/struct → trait/interface | — | Trait/interface implementation. |
| `uses` | reader → field/global | `sites: [...]` | A method reads a field; a function reads a global. |
| `reads` | statement → field/global | `sites: [...]` | Tier-2 only. Statement-level field reads. |
| `writes` | statement → field/global | `sites: [...]` | Tier-2 only. Statement-level field writes. |

### 4.3 Test edges

| Kind | Direction | Common attributes | Notes |
|---|---|---|---|
| `tests` | test_function → tested_symbol | `assertion_count?` | A test function exercises another symbol. Heuristic; confidence on edge. |
| `mocks` | test_function → mocked_symbol | — | A test mocks a symbol. |

### 4.4 Documentation edges (the doc-as-graph family)

| Kind | Direction | Common attributes | Notes |
|---|---|---|---|
| `documents` | doc_node → target_symbol | — | Docstring or doc-section explains a symbol. |
| `decides` | doc_node → target_symbol | `decision_status` (`active`/`superseded`/`rejected`) | A decision record applies to a symbol. |
| `references` | doc_node → target_symbol | `kind_hint` (`see_also`/`mentions`/`alternative`) | A doc node mentions but doesn't fully document a symbol. |
| `deprecates` | doc_node → target_symbol | `since_version?` | A doc node marks a symbol as deprecated. |
| `configures` | code_node → config_value | `read_at_runtime: bool` | Code reads a config value. |

### 4.5 Cross-language edges

Any edge can carry a `language_boundary: true` marker when src and dst
languages differ. The target may be an `unresolved_symbol` until the
cross-language binding is resolvable.

| Carrier edge | Common attributes |
|---|---|
| `calls`, `imports`, `uses` | `language_boundary: true`, `binding_kind` (`pyo3`/`napi`/`wasm-bindgen`/`ffi`/`unknown`) |

### 4.6 Edge attributes — common fields

| Field | Type | Always present |
|---|---|---|
| `kind` | enum (above) | ✅ |
| `src_id` | node ID | ✅ |
| `dst_id` | node ID | ✅ |
| `confidence` | u16 (milli-units, 0–1000) | ✅ |
| `source` | `structure` / `code` / `derived` | ✅ |
| `language_boundary` | bool | optional |
| `sites` | `[{line, ...}]` | optional, kind-specific |

## 5. Storage layout (Option C)

### 5.1 Directory shape

```
<repo>/
├── .aethyme/
│   ├── engine-version           ← plain text, committed
│   ├── graph/                   ← committed
│   │   ├── <source-path>.bin    ← per-file binary fragment (mirror source tree)
│   │   ├── ...
│   │   ├── _index/              ← committed per-module index shards
│   │   │   ├── <module>.ndjson
│   │   │   └── ...
│   │   └── _overlays/           ← committed non-per-file producer output
│   │       ├── <kind>.bin        (structure, configs, docs, risks, areas, …)
│   │       └── ...
│   ├── graph_store.redb         ← derived local redb query artifact
│   └── cache/                   ← gitignored
│       └── *.redb               ← future daemon-local mirrors
```

### 5.2 Per-file fragments

- Format: **binary (Bincode)**. One file per source file, mirroring the
  source tree.
- Contents: every node and edge whose canonical home is that source
  file. Cross-file edges live with the caller (see §4.1 ownership rule
  in the design doc).
- Records sorted canonically (kind, then ID) before serialization.

### 5.3 Per-module index shards

- Format: **NDJSON** (one record per line).
- Path: `.aethyme/graph/_index/<module>.ndjson`.
- Record shape:
  ```json
  {"module": "src.cli", "symbol": "explore_command", "kind": "function", "node_id": "fn:aethyme:...", "file": "src/cli.py"}
  ```
- Records sorted canonically by `(module, symbol, kind)` before writing.
- Multiple PRs adding distinct symbols to the same module produce
  mergeable changes via `merge=union` (§5.6).

### 5.4 Overlay fragments (non-per-file producer output)

Some producers don't naturally map to one source file. `structure`
classifies the whole repo into layers; `configs` aggregates every
config file into one normalized view; `docs` and `risks` synthesize
repo-level summaries. Forcing these into the per-file layout would be
a lie: the data has no single "home" source path.

Overlay fragments are the storage shape for that data.

- Path: `.aethyme/graph/_overlays/<kind>.bin`
- Format: **binary (Bincode)** — same wire format as per-file
  fragments, different envelope.
- Envelope (`OverlayFragment<P>`): `{ kind, schema_version,
  producer_version, payload }`.
- `kind` is the producer's logical name (`"structure"`, `"configs"`,
  `"docs"`, `"risks"`, `"areas"`, …) and is **also** the filename
  stem; the reader cross-checks them on decode (`KindMismatch`
  decode error if a file was renamed under the daemon).
- `schema_version` is the wire-envelope version — currently `1`.
  A bump is a forever-format-change event: the CI gate (§5.7)
  byte-compares regenerated fragments against committed ones, so
  the bump must land with a coordinated re-index. It is **not**
  the producer's logic version.
- `producer_version` is the producer's own version string
  (e.g. `"structure-producer/0.3.2"`); it is recorded for
  provenance and re-derivation triggers but is **not** load-bearing
  for decode.
- `payload` is the producer-defined type `P`. Each `<kind>.bin`
  carries its own payload type; there is no global discriminant.

**Why per-kind files instead of one giant enum.** A single
`OverlaySet` enum across all kinds would force every change to a
producer's payload to bump the global enum's bincode discriminant
space, fanning out a wire-format break to every other producer.
Per-kind files isolate that blast radius: a `risks` payload change
touches `risks.bin` only.

**Determinism contract is the same as §5.5.** Overlay payloads must
sort their internal collections canonically before serialization;
producers are responsible since the storage crate cannot inspect
`P`. The cross-construction byte-equality test in
`tests/disk.rs::overlay_bytes_on_disk_byte_identical_across_writes`
pins this for the envelope.

### 5.5 Determinism requirements

Mandatory across the indexing pipeline:

- Replace `HashMap` / `HashSet` with `IndexMap` / `IndexSet` (or `BTreeMap` / `BTreeSet`) anywhere a fragment payload is constructed.
- No timestamps, no PID, no random IDs in fragment payloads.
- No floating-point. Confidence scores are integers (milli-units, 0–1000).
- Canonical sort of every collection before serialization (alphabetical for strings, numeric for IDs).
- Parser version pinned in `.aethyme/engine-version`. CI fails on mismatch.

A non-determinism audit is part of the implementation milestone.

### 5.6 `.gitattributes`

```
.aethyme/graph/**/*.bin              linguist-generated=true
.aethyme/graph/**/*.bin              binary
.aethyme/graph/_index/**/*.ndjson    linguist-generated=true
.aethyme/graph/_index/**/*.ndjson    merge=union
.aethyme/engine-version              text
```

- `linguist-generated=true`: GitHub collapses graph diffs in PRs.
- `binary` on fragments: `git diff` shows changed/unchanged, no inline.
- The `**/*.bin` rules cover overlay files under `_overlays/` as well
  as per-file fragments.
- `merge=union` on index shards: auto-unions both sides' lines; canonical sort step in §5.5 deduplicates on the next re-index.

### 5.7 CI verification gate

PR verification, when enabled, is a byte-comparison gate over the
committed fragments:

1. Re-index from source in a clean working tree.
2. Compare the regenerated fragments byte-for-byte against committed ones.
3. Mismatch -> CI fails with "run `aethyme-graph-index` locally and commit
   the `.aethyme/graph/` result."

This is the trust boundary that prevents hand-edited or stale fragments
from being shipped.

### 5.8 Local redb query artifact (gitignored)

The current engine graph store is the Phase-3 redb file at
`.aethyme/graph_store.redb`. It is local-only and regenerated by
`aethyme-engine-cli index --repo <repo>`. Future daemon-local mirrors may
live under `.aethyme/cache/`, but committed fragments remain the source of
truth; redb files are performance/query artifacts.

Supported redb CLI surfaces are intentionally narrower than the fragment
schema:

- `aethyme-engine-cli index --repo <repo>`: rebuilds the local redb store
  from `.aethyme/graph/` fragments and writes snippets.
- `query-areas`: reads area rows from the redb store.
- `query-overview`: reads repo metadata, top areas, entrypoints, and risks.
- `deps`: reads outgoing file adjacency.
- `importers`: reads incoming file adjacency.
- `symbol` / `symbol-batch`: read bounded V2 function/class symbol matches
  from redb. Ranking uses exact-name, case-insensitive, prefix, snake/camel
  component, path-component, area, and basename signals. These commands do not
  build a `RepositoryMap`.
- `graph-node`, `graph-children`, `graph-parents`, `graph-callers`,
  `graph-callees`, `graph-docs`, and `graph-configs`: render node/relation
  views from read-only redb display and relation APIs while preserving the
  existing JSON shape. These commands do not build a `RepositoryMap`.
- `graph-expand`: composes the redb-backed node, relation, and risk views
  into the existing compact expand JSON shape. It preserves the existing
  per-relation bounds and does not build a `RepositoryMap`.
- `task-expand`: composes redb-backed callers/callees, docs/configs, and risk
  views into the existing compact task expansion JSON shape. It does not build
  a `RepositoryMap`.
- `task-anchors`, `task-scope`, `task-next`, and `task-localize`: read task
  anchors, scope, and navigation order from redb overview rows, path indexes,
  bounded symbol candidates, relation views, config/doc rows, and risk rows.
  Task policy and ranking stay in the graph modules. These commands preserve
  the existing JSON shape and do not build a `RepositoryMap`.
- `analyze-usage-boundary`: reads public PHP symbol seeds and candidate
  source/docs/config files from redb path indexes and adjacency, then scans
  source/docs/config text for evidence. This hybrid contract is intentional:
  evidence strings are freshness-sensitive, and query-time text scanning avoids
  trusting stale redb evidence rows. It preserves usage-boundary policy in
  graph modules and does not build a `RepositoryMap`.
- `callers`: uses the current hybrid path: grep for the symbol, then use
  redb adjacency to expand candidate files.

The redb store is not the durable graph format. It does not make
`.aethyme/graph_store.redb` a committed artifact, and it does not replace
fragments as the durable graph. Broader redb-backed reads must still treat
fragments as the regeneration source.

Current redb storage coverage:

- Schema version `5` means `aethyme-engine-cli index` populates typed rows
  for repositories, directories, files, areas, functions, classes, docs,
  configs, unresolved/import placeholders, and risks.
- `SYMBOL_BY_NAME` is populated for function/class simple names using
  ASCII-lowercased exact-name keys. `SYMBOL_BY_COMPONENT` is populated for
  lowercased function/class name components. `SYMBOL_BY_PATH_COMPONENT` is
  populated for lowercased function/class file-path components.
  `FUNCTIONS_BY_PATH` is populated for file-scoped function lookup.
  `NODES_BY_PATH` is the broader path index for directories, files, functions,
  classes, docs, configs, and unresolved/import placeholders.
- The writer persists the graph edge set without skipping edges for missing
  unresolved/import endpoint rows. Placeholder endpoints are stored as typed
  unresolved rows before adjacency is written.
- Graph overview, task/context-pack assembly, activation, task-localize,
  task-expand, non-usage-boundary `explore`, and usage-boundary seed discovery
  read from redb. Usage-boundary remains hybrid: redb supplies public-symbol
  and candidate-file seeds, while source/docs/config text still supplies
  evidence.

Usage-boundary Phase 5 decision:

- **Accepted V2 contract:** hybrid redb + source text. redb discovers the
  bounded candidate set; source/docs/config text supplies evidence strings at
  query time.
- **Rationale:** evidence spans are freshness-sensitive. A stale persisted
  evidence row can produce a wrong removal recommendation, while a fresh text
  scan over redb-discovered files is slower but safer.
- **Fully redb-native remains future work:** only revisit if the indexer
  persists usage evidence rows with symbol id, caller file, line/span, evidence
  kind, and internal/external classification hints, plus explicit
  freshness/invalidation rules.

Remaining V2 redb store contract:

- Ownership is unchanged. `.aethyme/graph/` fragments are the source of
  truth; `.aethyme/graph_store.redb` remains a derived, local, disposable
  query artifact rebuilt from those fragments.
- V2 must persist any remaining graph-navigation node kinds: separately
  modeled methods if they stop being represented as functions, modules if
  they are introduced as separate containers, and any future container rows
  needed for prefix or parent/child navigation. Their ids must still be
  derived from canonical fragment data.
- Each persisted node row must retain the canonical id, kind, path/name,
  language, range, area/container membership, and other typed fields needed
  to derive labels and navigation output. V2 must not introduce stored
  display labels as a substitute for canonical fields.
- V2 must persist the full edge set used by graph navigation. The current
  writer includes persisted repository, directory, file, function, class, doc,
  config, and unresolved endpoints; any future node kind needs a matching
  typed row before its edges are advertised as redb-backed.
- Both outgoing and incoming adjacency indexes are required for every
  persisted edge kind. A V2 writer must not silently drop symbol-level edges
  and still advertise graph-navigation coverage.

Required V2 read APIs for replacing `RepositoryMap` reads:

| API family | Required capability | Current graph-module consumer |
|---|---|---|
| Node lookup | Implemented as `get_node(id)`, `get_nodes(ids)`, `node_display(id)`, and `area_for_node(id/path)` for typed repository/directory/file/area/function/class/doc/config/unresolved rows. | `node_view`, relation rendering, risk/doc/config display. |
| Relation lookup | Implemented as `children(id, kind?)`, `parents(id, kind?)`, `relation_view(id, relation)`, `docs_for(id)`, `configs_for(id)`, and `risk_for_node_or_path(id/path)`. | `children_view`, `parents_view`, `docs_view`, `configs_view`, `graph_expand_view`, `task_expand_view`. |
| Symbol lookup | Implemented as `find_symbols(name, kind?)` for exact lookup and `symbols_matching(query)` / `symbols_matching_with(...)` for bounded exact, case-insensitive, prefix, component, path-component, area, and basename-signal candidates. `symbols_matching*` returns store-ranked candidates; higher-level task flows may still add task-specific ranking above those rows. | `task_anchors_view`, symbol query surfaces, graph target resolution. |
| Path prefix lookup | Implemented as `nodes_under_path(prefix)`, `functions_under_path(prefix)`, and `resolve_file_path(path)` over redb path indexes. | Scope expansion, area views, task-local navigation seeds. |
| Adjacency | Implemented as `neighbors(id, direction, kind?)`; existing `edges_from` / `edges_to` remain compatibility wrappers for unfiltered directions. | `children_view`, `parents_view`, `callers_view`, `callees_view`, `docs_view`, `configs_view`, `task_expand_view`, `graph_expand_view`. |
| Task anchor candidates | Implemented as `task_anchor_candidates(task_tokens, limit)` and `symbols_matching_with(...)`, returning bounded typed candidates with exact/prefix/component/path/area signals. Ranking stays in the graph module. | `task_anchors_view`, `task_scope_view`, `task_next_view`, context-pack assembly. |
| Usage-boundary seeds | Implemented through redb path/adjacency APIs: `functions_under_path`, `nodes_under_path`, `neighbors`, plus bounded `usage_boundary_candidates(scope, symbol_kind, limit)` for consumers that need capped seeds. | `usage_boundary_query`, `analyze-usage-boundary`, dead-code analysis seed selection. |
| Overview/navigation slices | Implemented as `overview_v2(...)` for bounded repo metadata, repository, directories, areas, entrypoints, risks, files, functions, classes, docs, configs, and unresolved placeholders. `graph_overview_view_redb`, `task_next_view_redb`, native `explore`, and redb context-pack assembly adapt these rows directly. | `graph_overview_view`, `task_next_view`, `explore`, context-pack navigation order. |

Bridge decisions as of 2026-07-17:

| Consumer | Decision | Current status |
|---|---|---|
| `symbol` / `symbol-batch` | Redb-backed V2 symbol search. | CLI opens `.aethyme/graph_store.redb` read-only and serves bounded function/class matches through `symbols_matching_with`; it fails cleanly when the store is missing and no longer builds `RepositoryMap` for fuzzy scoring. |
| `deps` / `importers` | Redb-backed equivalent. | CLI reads `neighbors(id, Outgoing/Incoming, None)` and preserves the existing path-list output. |
| `query-overview` | Redb-backed equivalent with V1 JSON projection. | CLI reads `overview_v2(Default)` but still emits only the stable `repo`, `areas`, `entrypoints`, and `risks` keys. |
| `graph-overview` | Redb-backed rendered overview. | CLI opens `.aethyme/graph_store.redb` read-only, renders the existing overview JSON shape from `overview_v2(...)`, and has tiny/medium parity plus deterministic-query gates. |
| `callers` | Keep the hybrid grep + redb adjacency path. | Grep finds candidate files containing the symbol; redb `neighbors(..., Incoming, None)` expands importers before line grep. |
| `task-anchors` | Redb-backed task anchors. | CLI opens `.aethyme/graph_store.redb` read-only, resolves anchors from overview rows, path indexes, config/doc rows, and bounded symbol candidates, then applies graph-module task ranking. Binary parity snapshots cover ExplainRepo, ChangeSymbol, TraceImpact, and config-ownership tasks. |
| `task-scope` | Redb-backed task scope. | CLI composes redb-backed anchors with redb path-prefix lookup, symbol rows, area membership, and risk lookup while preserving the existing JSON shape. Binary parity snapshots cover ExplainRepo, ChangeSymbol, TraceImpact, and config-ownership tasks. |
| `task-next` / `task-localize` | Redb-backed task navigation. | `task-next` reads redb-backed anchors, relation views, semantic config/doc path rows, and bounded overview/navigation slices. `task-localize` composes `task-anchors`, `task-scope`, and `task-next`; `--profile` reports redb stages rather than `RepositoryMap` build time. |
| `task-expand` | Redb-backed task expansion. | `task-expand` opens `.aethyme/graph_store.redb` read-only, composes callers/callees, docs/configs, and risks into the existing compact JSON shape, and has binary parity coverage against `RepositoryMap` snapshots. |
| `pack` / `task-pack` / `context` / `task-context` / `explain` / `task-explain` | Redb-backed context-pack assembly. | CLI opens `.aethyme/graph_store.redb` read-only to select anchors, scope, docs/configs, risks, symbols, relations, and path rows. Source text is read only to supply snippets/content. RepositoryMap remains only as a test oracle for parity and token-regression gates. |
| `activate` / `activate-from` / `impact` | Redb-backed activation and neighborhood views. | CLI opens `.aethyme/graph_store.redb` read-only and expands from redb anchors, adjacency, docs/configs, call/import relations, area, and risk projections. |
| `graph-node` / rendered relation views | Redb-backed rendered views. | `graph-node`, `graph-children`, `graph-parents`, `graph-callers`, `graph-callees`, `graph-docs`, and `graph-configs` open `.aethyme/graph_store.redb` read-only, adapt redb display/relation rows to the existing JSON structs, and have binary parity snapshots against `RepositoryMap`. |
| `graph-expand` | Redb-backed composed view. | `graph-expand` opens `.aethyme/graph_store.redb` read-only, composes the redb-backed node, parents, children, callers, callees, docs, configs, and risks views, preserves the existing JSON shape and bounds, and has binary parity, shape, bounded-output, deterministic-ordering, and docs/configs/call-edge fixture gates. |
| `explore` non-usage-boundary intents | Redb-backed native explore. | `task_localization_query`, `behavior_localization_query`, and auto-selected native intents read graph/navigation data from `.aethyme/graph_store.redb`; observability reports redb store status/freshness. |
| `usage boundary` | Hybrid redb + source text. | `analyze-usage-boundary` and `explore --intent usage_boundary_query` open `.aethyme/graph_store.redb` read-only, discover public PHP symbols and candidate files through redb path indexes/adjacency, then scan source/docs/config text for evidence. This is the accepted Phase 5 contract; fully redb-native evidence would require persisted evidence rows plus freshness/invalidation rules. |
| Trait abstraction | Defer. | There is no production dual backend for migrated surfaces. RepositoryMap is kept only where still actively required outside these redb-backed commands, or as an explicit test oracle. |

V2 correctness and performance gates:

- Tiny read-API gates live in
  `rust/crates/aethyme-engine/src/store/redb/graph_store.rs` and cover
  `get_node`, `get_nodes`, `node_display`, `area_for_node`, `find_symbols`,
  `symbols_matching`, `symbols_matching_with`, `nodes_under_path`,
  `functions_under_path`, `resolve_file_path`, `neighbors`, `children`,
  `parents`, `relation_view`, `docs_for`, `configs_for`,
  `risk_for_node_or_path`, `task_anchor_candidates`,
  `usage_boundary_candidates`, and `overview_v2` on a tiny typed store
  fixture.
- Binary integration gates live in
  `rust/crates/aethyme-engine/tests/redb_cli.rs` and cover indexing from
  fragments, redb exact/fuzzy symbol query, graph callers/callees query
  shape, rendered graph parity snapshots, graph overview parity/shape/
  determinism, graph-expand JSON shape and bounded output, task-expand
  relation parity, task parity snapshots for ExplainRepo, ChangeSymbol,
  TraceImpact, and config-ownership tasks, context-pack/context/explain
  aliases, activation/activate-from/impact, native explore intents,
  usage-boundary internal/external caller plus docs/config reference behavior,
  usage-boundary explore, usage-boundary source-change freshness,
  deterministic query snapshots after rebuilding from identical fragments,
  missing-store read-only behavior, disposable-fast publish boundaries, and a
  medium fixture that runs index, symbol, graph views, graph overview,
  graph-expand, task views, context pack, activation, usage-boundary, and
  explore together.
- The default performance smoke is intentionally tiny and bounded by
  environment-overridable thresholds:
  `AETHYME_REDB_PERF_MAX_INDEX_MS`,
  `AETHYME_REDB_PERF_MAX_QUERY_OVERVIEW_MS`, and
  `AETHYME_REDB_PERF_MAX_SYMBOL_MS`.
- MediaWiki-scale verification is an explicit ignored Rust test:
  `cargo test -p aethyme-engine --test redb_cli mediawiki_scale_redb_smoke_for_v2_paths -- --ignored --nocapture`
  with `AETHYME_MEDIAWIKI_REPO=/path/to/mediawiki`. Run it when broadening
  redb-backed graph paths beyond the query surfaces listed above. Thresholds
  are configurable with
  `AETHYME_REDB_MEDIAWIKI_MAX_INDEX_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_QUERY_OVERVIEW_MS`, and
  `AETHYME_REDB_MEDIAWIKI_MAX_SYMBOL_MS`. The broad recall sanity check uses
  `AETHYME_REDB_MEDIAWIKI_MAX_BROAD_SYMBOL_MS`, plus
  `AETHYME_REDB_MEDIAWIKI_MAX_GRAPH_OVERVIEW_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_RELATION_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_GRAPH_EXPAND_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_TASK_ANCHORS_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_TASK_SCOPE_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_TASK_NEXT_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_TASK_LOCALIZE_MS`,
  `AETHYME_REDB_MEDIAWIKI_MAX_CONTEXT_PACK_MS`, and
  `AETHYME_REDB_MEDIAWIKI_MAX_EXPLORE_MS` for the Phase 6 navigation smoke.
- Playground eval/token verification is also an explicit ignored Rust test:
  `cargo test -p aethyme-engine --test redb_cli playground_context_pack_token_regression_gate_never_self_eval -- --ignored --nocapture`
  with `AETHYME_PLAYGROUND_REPO=/path/to/playground`. The test canonicalizes
  the supplied repo and fails if it points inside the Aethyme checkout. It
  compares redb context-pack and task-pack budget metrics against the
  RepositoryMap test oracle: token estimate, selected file count, selected
  symbol count, snippet count, command-output chars, and `.aethyme` path-leak
  status. It then runs router-level `aethyme explore` and records that Explore
  was actually invoked. The gate intentionally does not require exact selected
  file equality, because ranking churn can be acceptable while budget signals
  stay healthy. This is the first real consumer-facing token regression gate;
  it is never an Aethyme self-eval.

V2 redb-backed navigation surfaces should be described as complete only after
they read from `.aethyme/graph_store.redb` without constructing a full
`RepositoryMap` and have binary gates for shape, determinism, and missing-store
behavior. Usage-boundary is complete under the hybrid V2 contract above: it is
redb-seeded, but intentionally scans source/docs/config text for evidence. A
future fully redb-native analyzer would need its own evidence model and
freshness gates before replacing that source-text pass.

## 6. Update propagation

V1 propagation is explicit indexing. There is no supported Python daemon
contract that owns graph fragments or keeps `.aethyme/graph_store.redb`
live.

| Trigger | Effect |
|---|---|
| `aethyme-graph-index --repo-root <repo> ...` | Rebuilds committed source-of-truth fragments and index shards under `.aethyme/graph/` |
| `aethyme-engine-cli index --repo <repo>` | Rebuilds the derived local `.aethyme/graph_store.redb` query artifact from `.aethyme/graph/` |
| Playground setup / verification scripts | Assert both `.aethyme/graph/` and `.aethyme/graph_store.redb` exist after the two-step build |
| Future watch/daemon work | Must preserve the same ownership split: fragments remain the durable graph; redb remains derived and rebuildable |

## 7. Modification history

Each node carries:

```
modification_history: [
  { commit: "abc123", timestamp: "2026-05-14T10:00:00Z", author: "..." },
  { commit: "def456", timestamp: "2026-05-12T14:30:00Z", author: "..." },
  { commit: "789xyz", timestamp: "2026-05-10T09:15:00Z", author: "..." },
]
```

Fixed-size ring buffer of **3 entries**, evict-oldest. Captures recency
(when was this last touched), the prior touch (was the most recent edit
a follow-up?), and one older anchor. Storage cost is ~150 bytes per
node regardless of repo age.

`change_frequency` is a derived fact computed at index time from the
ring buffer plus the git log over a fixed recency window (90 days). Not
stored on the node; recomputed each re-index.

## 8. Cross-language conventions

### 8.1 Neutral kinds, language extensions

Every node kind is language-neutral. Language-specific facts live in:

```
extensions: {
  python: { decorators: [...], is_async: true },
  rust:   { lifetimes: [...], is_unsafe: false, attributes: [...] },
  typescript: { generics: [...], modifiers: [...] }
}
```

Only the language(s) that have facts populate their namespace. Consumers
that don't care about language-specifics ignore `extensions`. Consumers
that do reach into the relevant namespace.

### 8.2 Edge vocabulary is uniform

`calls`, `inherits`, `implements`, `imports`, `uses` mean the same thing
across languages. When semantics diverge subtly (e.g. Python's `super()`
vs. Rust trait dispatch), the difference is captured in edge attributes
under `extensions.<lang>` or as a `binding_kind` attribute.

### 8.3 Cross-language query results

Results are uniformly neutral-kind. A query that traverses Python →
Rust returns nodes of mixed language; each node carries its own
`extensions.<lang>`. No vocabulary coercion at the boundary.

## 9. Derived facts catalog (built-in)

All materialized at index time. No user-defined predicates.

| Fact | On which kinds | Computed from |
|---|---|---|
| `in_degree` | every node with incoming edges | reverse index count |
| `out_degree` | every node with outgoing edges | forward index count |
| `is_unused` | callable kinds | `out_degree` of `calls` reverse edges == 0 AND `visibility == public` AND no `tests` edges |
| `is_public_api` | callable kinds | `visibility == public` AND `enclosing_module` is exported |
| `is_test_adjacent` | every node | reachable via `tests`/`mocks` edges within 1 hop |
| `change_frequency` | every node | derived from `modification_history` + git log window |
| `has_docstring` | callable/class/module kinds | `documents` reverse edge exists |

## 10. Open items for the implementation phase

These are intentionally deferred to the implementation milestone, not
the schema lock:

- **Initial bootstrap.** When an existing repo onboards Aethyme: single
  bulk-index commit, or chunked rollout? Probably single commit with
  `[skip ci]` for the verification pass.
- **History squash policy.** Whether graph commits get squashed
  separately from source commits in PR merges. Probably "merge as-is"
  to preserve incremental authorship.
- **Edge-attribute schema details.** The exact field set inside
  `sites[]` for each behavioral edge kind. Need a pass per language
  parser.
- **Extension-field schemas per language.** Python, Rust, TypeScript,
  PHP — each gets its own pass to define what's worth promoting into
  `extensions.<lang>`.
- **Snapshot bundle (Option B from design conversation).** If
  zero-cold-start ever becomes a requirement, ship a snapshot-export
  command. Not on the v1 critical path.

## Cross-references

- Design conversation: see commit log around 2026-05-14 for the 7-topic
  decision thread.
- Cross-process consumers: this schema's wire shape is a new entry on
  the [`cross-process-consumers.md`](cross-process-consumers.md)
  registry — every kind, edge kind, and required field becomes a
  tracked symbol.
- Engine redb materialization: [Phase 3 redb plan](phase3-redb-graph-store-plan.md)
  explains the historical SurrealDB-to-redb migration. The current redb
  file is `.aethyme/graph_store.redb`; committed fragments under
  `.aethyme/graph/` are the source of truth.
- BSL constraint: [Phase 3 redb plan](phase3-redb-graph-store-plan.md)
  explains why the cache layer must stay BSL-free; the same constraint
  applies to any library used to write/read fragments.
