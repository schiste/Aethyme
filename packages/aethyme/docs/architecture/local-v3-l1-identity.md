# Local v3 — L1 identity decision record (#652)

Last Updated: 2026-10-16

This record is input to decision **D10** (how identities and exact revision references are
minted and encoded) and the identity part of **D39** (how existing NodeIds relate to
portable references). It is a **proposal**, except where "Decided" below says otherwise.
D10 closes only with the evidence the plan names (two clones, rename/copy/split,
rationale-only updates, canonical Rust/TypeScript fixtures, authority binding), and part
of that evidence comes from E1 (#650), which has not run.

Baseline facts come from the L0 audit (`local-v3-l0/`, slice D): NodeId hashes
(repo, path, name, kind) and nothing else; there is no FileId; the engine also mints
path-string IDs (`file:`, `dir:`, …); the repository key is derived from the repository's
path; and the existing source digest hashes Git object ids.

The D39 source review pins an additional detail: `NodeId::new` hashes the caller-supplied
repo component without canonicalizing it. The engine's filesystem adapter supplies the
canonical checkout basename, while `IndexerContext` retains its caller-provided repo
name. Two clones named `clone-a` and `clone-b` therefore mint different existing
NodeIds for the same path, name and kind. Body edits preserve those IDs; path moves and
renames change them. These are existing semantics and remain unchanged.

## Status per identity

| Identity | Status | Where |
|---|---|---|
| `SourceSnapshotId` | **Implemented, experimental v0.** Golden vectors from an independent implementation. | `aethyme-contracts::experimental_v0::source_snapshot` |
| `NodeId` (existing) | **Unchanged.** Body and signature independence now pinned by a test (T65 baseline, #700). | `aethyme-graph-indexer/tests/rust_indexer.rs` |
| `ProjectId` | Proposed below; no code. | — |
| `FileId` | Proposed below; rename/copy/split rules **wait for E1** (FX02 move+edit). | — |
| `ContributionId` / `ContributionRevisionId` | Proposed below; lineage shape **waits for E1** (FX03 inherited base, FX07 subtraction). | — |
| `BrokerId` / `SessionRef` | Proposed below; no code. | — |
| `SymbolRef` | Experimental scoped records and a finite advisory mapping resolver are implemented. No producer emits these records automatically. | `aethyme-contracts::experimental_v0::symbol_identity`; `aethyme-broker::experimental_symbol_mapping` |

## Common rules

- **Encoding.** Every identity is a self-describing string `<scheme>:<value>`. A digest
  identity names its algorithm (`sha256:…`), so a future algorithm is a new tag and never a
  silent re-identification. Parsers accept only the canonical form; they never normalize.
- **Hash.** SHA-256, so that independent implementations (a Worker via WebCrypto, a
  third party with `shasum`) can verify without a dependency.
- **Locators are not identities.** Branch names, URLs, paths, session numbers and symbol
  labels are aliases. None alone proves two records refer to the same source or authority.
- **Failure response.** Resolution answers `missing`, `ambiguous` or an explicit
  conflict; it never guesses.

## Identities

### SourceSnapshotId: implemented (experimental v0)

| | |
|---|---|
| Minting authority | None; derived. Anyone with the bytes computes the same ID. |
| Scope | Global. It identifies bytes, not a project or an authority. |
| Immutable preimage | Domain header, then per entry `mode SP sha256(content) SP path NUL`, sorted by raw path bytes. See the module docs. |
| Encoding | `sha256:<64 lowercase hex>` |
| Accepted | Regular `100644`, executable `100755`, symlink `120000`. Raw path bytes, including non-UTF-8 paths. |
| Refused | Submodules and other modes; empty, absolute, `.`/`..`, `.git` and every name a filesystem resolves to it (any case; HFS+ ignorable code points; NTFS trailing dots or spaces, `:` streams and `git~1`, as Git's `core.protectHFS`/`protectNTFS` refuse), NUL, `//`, trailing `/`, paths over 4096 bytes; duplicates; file/directory conflicts. Nothing is normalized. |
| Case and composition | **Faithful (decided 2026-10-09).** Paths differing only by letter case or Unicode composition are distinct, valid entries, as in Git. Materialization (#670) must refuse such collisions explicitly on a case-insensitive or normalizing filesystem. |
| Not | A Git object id, the graph manifest digest, or execution identity (#670). |

### ProjectId: proposed

- **Minted** once, at random, in an explicit enrollment descriptor (`proj:<random 128-bit,
  base32>`). Not derived from URL, path or tree; the current repository key is path-derived
  and stays a local storage key only.
- **Two clones** enrolled independently get two ProjectIds. They are bound only by an
  authenticated, reviewed association record, never by a matching URL or tree (T03).
- **Lifecycle:** a rebind is an explicit new descriptor revision, never an edit in place.

### FileId: proposed; waits for E1

- **Existing files at enrollment:** derived from (ProjectId, enrollment SourceSnapshotId,
  raw path bytes). The same enrollment always yields the same FileIds.
- **New files:** minted at random in the proposal that creates them. Two proposals creating
  the same path is a namespace **conflict**, never resolved by timestamp.
- **Rename:** an explicit, validated rename keeps the FileId. An inferred rename is
  recorded as `uncertain` until confirmed. Copy and split create new FileIds linked by
  `derived_from`.
- **Waits for E1:** whether "validated" can rest on the composer's correspondence evidence
  (FX02 move+edit) or needs an explicit operation from the author.

### ContributionId and ContributionRevisionId: proposed; waits for E1

- **ContributionId:** minted at random for one logical submitted intent under an
  authorized objective. Not a task ID, and not a promise that the contribution can later be
  removed from a synthesized descendant.
- **ContributionRevisionId:** a digest over the canonical record of one immutable revision:
  exact base and result `SourceSnapshotId`s, declared dependencies (as revision IDs),
  brief, and metadata. A metadata- or brief-only change is a new revision. Consumers pin a
  revision, never "latest". The record layout is #653's.
- **Waits for E1:** which lineage the record must carry so that an inherited contribution
  is not applied twice (FX03), and whether subtraction (FX07) needs more than the original
  revisions.

### BrokerId and SessionRef: proposed

- **BrokerId** is minted at random per broker installation. **SessionRef** is
  (BrokerId, local integer session id). The integer alone is neither unique nor a
  permission, and broker session ids are already per repository.

### SymbolRef: experimental v0; producer integration remains out of scope

- `SymbolRef` wraps the unchanged existing NodeId with ProjectId, clone identity, and an
  explicit engine-node scope. Producer-local scope includes producer namespace, document
  and local label; external package scope retains producer, package, version and symbol.
  Unknown scope kinds are retained as opaque data and cannot resolve.
- `ObservedSymbolRevision` binds one SymbolRef to an exact SourceSnapshotId, profile,
  fact revision and origin record. `SymbolOriginEvidence` and
  `ExplicitSymbolMappingEvidence` are advisory records. A `SymbolMappingClaim` has only
  the wire states `proposed` and `unknown`; a serialized `established` value is
  unrecognized.
- The broker returns its non-serializable `EstablishedWithinBinding` only when a local
  binding token matches the exact project, two distinct clone IDs, both source snapshot
  IDs and one profile, and the claim resolves to exactly seven canonical records:
  mapping evidence, two observations, two symbol refs and two distinct origin records.
  Missing, duplicate, aliased, malformed, cross-scope or mismatched evidence yields
  `Unknown`. This result is process-local and advisory; it is not verification, accepted
  identity, or execution authority. No name, NodeId equality or contributor claim alone
  creates it.
- External package mappings must retain the same producer, package and exact version on
  both sides. A proposed cross-version mapping remains `Unknown` because this adapter has
  no evidence that proves compatibility between package releases.
- Mapping scope is intentionally bounded. The two-clone fixture checks body edits,
  path moves, renames, copies, equal producer-local labels in separate documents,
  external package versions, and historical overload collisions. In particular, the
  legacy NodeId tuple omits signature, so overload declarations with the same repo, path,
  name and kind collide; without explicit resolved evidence the mapping remains
  `Unknown`. Old NodeIds are not rewritten.
- The adapter validates NodeId syntax and repo-component binding for engine scopes. It
  does not execute an indexer, infer clone lineage, prove external package contents, or
  turn a proposed record into cross-version identity. There is no automatic producer.
- **Body revision is separate.** The T65 test shows that a function's NodeId survives body
  **and signature** edits, so "same NodeId" never means "same code". Any reuse of a result
  binds the exact SourceSnapshotId (#655, #685).
- Cross-clone or cross-version correspondence is an explicit mapping record; when
  uncertain, it says so (T66).

## Decided

- **Case-fold and composition collisions (2026-10-09): faithful identity, refuse at
  materialization.** Identity describes the tree, not a host; every valid Git tree gets
  an ID (real repositories contain such pairs, e.g. the Linux kernel's `xt_TCPMSS.c` and
  `xt_tcpmss.c`). The golden vector "case-differing paths" pins acceptance. The
  obligation moves to #670: a checkout onto APFS/NTFS must detect collisions under that
  filesystem's folding and normalization and fail explicitly, never keep one file.
- **Common encoding rules (2026-10-09): accepted as the D10 direction.** Self-describing
  `<scheme>:` identities, algorithm-tagged SHA-256 digests, canonical-only parsing, and
  locators kept separate from identities. #653 (records) and #655 (analysis envelope)
  build on them. D10 itself still closes only with the E1 evidence named above.

## Tests

| Plan test | State |
|---|---|
| T01 (canonical golden vectors) | `SourceSnapshotId` part done in Rust: `aethyme-contracts/fixtures/experimental-v0/source_snapshot.json`, produced by an independent implementation and cross-checked with `shasum`. TypeScript side pending a TS consumer. Record vectors are #653's. |
| T03 (two clones) | Not started: needs the enrollment descriptor. |
| T05 (rename, copy, split, competing paths) | Not started: waits for E1. |
| T65 (body edit keeps NodeId, revision changes) | Baseline done (#700). The "revision binds the exact input" half is #655's envelope. |
| T66 (scoped symbols, clone mapping) | Experimental records, resolver and an actual two-clone synthetic Playground fixture are added under AQ0. The fixture covers scope mismatches, independent origins, stale snapshots, external version mismatch and overload collisions. Runtime qualification remains pending; no producer integration is claimed. |
