# Aethyme Graph Schema

Last Updated: 2026-05-14

Status: **Design — not yet implemented.** Locks the schema shape for the
next implementation phase. No schema versioning means every decision here
is forever; push back hard before any code lands against this doc.

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
| **Index is a tracked artifact, not a cache** | Per-file fragments and per-module index shards live under `.aethyme/graph/` in git. The daemon's redb mirror lives under `.aethyme/cache/` (gitignored). |
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
│   └── cache/                   ← gitignored
│       └── *.redb               ← daemon's live mirror
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

PRs are verified by `aethyme index --verify`:

1. Re-index from source in a clean working tree.
2. Compare the regenerated fragments byte-for-byte against committed ones.
3. Mismatch → CI fails with "run `aethyme index` locally and commit the result."

This is the trust boundary that prevents hand-edited or stale fragments
from being shipped.

### 5.8 Daemon cache (gitignored)

The daemon mirrors the committed fragments into a redb store under
`.aethyme/cache/` for fast query-time access. The cache is local-only,
regenerated incrementally from fragments. Fragments are the source of
truth; the cache is a performance artifact.

## 6. Update propagation

Hybrid: live in-memory + commit-time persistence.

| Trigger | Effect |
|---|---|
| File save (file-watch, debounced ~200ms) | Daemon re-parses the file; updates in-memory graph; live queries see the change |
| Commit (pre-commit hook) | Daemon writes the affected fragments and index shards under `.aethyme/graph/` |
| `aethyme index` (manual) | Full re-index pass; writes all fragments deterministically |
| `aethyme index --verify` (CI) | Re-index in a clean tree; compare against committed fragments |

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
- Engine cache (existing): [Phase 3 redb plan](phase3-redb-graph-store-plan.md) — the daemon cache layer under `.aethyme/cache/` reuses Phase 3's
  on-disk patterns; the committed fragments under `.aethyme/graph/`
  are new.
- BSL constraint: [Phase 3 redb plan](phase3-redb-graph-store-plan.md)
  explains why the cache layer must stay BSL-free; the same constraint
  applies to any library used to write/read fragments.
