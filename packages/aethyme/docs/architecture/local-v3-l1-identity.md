# Local v3 — L1 identity decision record (#652)

Last Updated: 2026-11-03

This record is input to decision **D10** (how identities and exact revision references are
minted and encoded) and the identity part of **D39** (how existing NodeIds relate to
portable references). It is a **proposal**, except where "Decided" below says otherwise.
At the original L1 proposal freeze, E1 (#650) had not yet run. The later bounded E1
campaign (#755), described in the [E1 profile](local-v3-e1-profile.md), qualifies a
retained HTML move/edit profile and specific synthesis, consumer and subtraction paths.
That campaign did not itself establish generic ProjectId enrollment, FileId transitions
or contribution lineage. Those are implemented separately as experimental v0 (commit
`324a49320`) and qualified in the [L1 identity evidence](local-v3-qualification/l1-identity-2026-11-03/README.md).
D10 is closed below.

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
| `ProjectId` | **Implemented, experimental v0.** Enrollment and approved descriptor binding; T03 qualified. | `aethyme-contracts::experimental_v0::project_identity`; `aethyme-broker::collaboration_cli::project` |
| `ProjectAssociation` | **Implemented, experimental v0.** Exact-record proposal, local confirmation, revocation and fail-closed mapping resolution; T03 qualified. | `aethyme-contracts::experimental_v0::project_association`; `aethyme-broker::collaboration_cli::project_association`; `aethyme-broker::experimental_symbol_mapping` |
| `FileId` | **Implemented, experimental v0.** Enrollment, explicit/uncertain rename, copy, split and competing-path rules; T05 qualified. | `aethyme-contracts::experimental_v0::project_identity`; `aethyme-broker::collaboration_identity_store` |
| `ContributionId` / `ContributionRevisionId` | **Implemented, experimental v0.** Immutable revisions with exact base/result, file changes, brief and dependencies. | `aethyme-contracts::experimental_v0::contribution_revision`; `aethyme-broker::collaboration_cli::contribution` |
| `BrokerId` / `SessionRef` | **Deferred.** No portable record consumes them yet; see below. | — |
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

### ProjectId: experimental v0

- **Minted** once, at random, in an explicit enrollment descriptor (`proj:<random 128-bit,
  base32>`). Not derived from URL, path or tree; the current repository key is path-derived
  and stays a local storage key only.
- **Two clones** enrolled independently get two ProjectIds. They can be associated only
  by an exact operator-confirmed record that rechecks both committed descriptors and both
  independently approved local bindings, never by a matching URL or tree.
- **Lifecycle:** a rebind is an explicit new descriptor revision, never an edit in place.

### ProjectAssociation: experimental v0

- `aethyme collab project association propose --peer-repo <path> [--write]` names the
  exact local and peer ProjectKeys, ProjectIds, committed descriptor RecordIds and anchor
  snapshots. Every explicit proposal also has a bounded `sha256:<64 lowercase hex>` nonce
  in its canonical record, so proposing the same bound pair later yields a new exact
  RecordId. The nonce is not secret and grants no authority. A proposal file changes no
  collaboration store.
- `confirm --peer-repo <path> --confirm <record-id>` revalidates both current committed
  descriptor pointers and both independently approved bindings, then writes one canonical
  object and one row in the invoking project's existing identity/object store. The peer
  remains read-only; confirmation does not create a cross-database transaction.
- `revoke --confirm <record-id>` changes only the local row. Mapping resolution reloads
  that canonical row and both approved bindings on each call; stale, missing, revoked,
  ambiguous, unavailable or wrong-side state resolves as unknown.
- Revoked association records remain tombstones and cannot be reactivated. To associate
  the same pair again, move or remove the old proposal file, create a fresh proposal and
  explicitly confirm its new RecordId; the old RecordId remains revoked.
- Associations grant no source permission, execution, acceptance or transitive authority.

### FileId: experimental v0

- **Existing files at enrollment:** `file:sha256:` over a domain header, ProjectId,
  enrollment SourceSnapshotId and length-prefixed raw path bytes (`FileId::enrolled`).
  The same enrollment always yields the same FileIds; a different ProjectId never does.
  Clones that explicitly bind one approved descriptor therefore share FileIds, and a clone
  that enrolls on its own does not, even with the same origin URL and bytes (T03).
- **New files:** derived deterministically from (ProjectId, proposal id, result
  SourceSnapshotId, ordinal, path) under a separate domain (`FileId::proposed`). The
  proposal id is unique per proposal, so IDs never collide across proposals, and an exact
  retry reproduces the same IDs instead of minting new ones. (An earlier draft of this record
  said "at random"; determinism was chosen so replay is idempotent.) Two proposals creating
  the same path are a namespace **conflict** (`CompetingPath`), never resolved by timestamp,
  even with equal bytes.
- **Rename:** an explicit, validated, unedited rename keeps the FileId (`ExplicitRename`).
  An unannotated delete/add pair is never treated as a rename; an exact-content match is
  recorded as `UncertainRename` until confirmed, and an edited rename is not claimed as
  validated. Copy and split create new FileIds linked by `derived_from`.
- **Replay:** foreign-project state and a wrong base snapshot are refused; competing
  histories resolve as unknown (`collaboration_identity_store`).
- **Out of scope:** cross-project FileId correspondence. Two separate projects are related
  only through a `ProjectAssociation` and explicit symbol mapping records, never by path.

### ContributionId and ContributionRevisionId: experimental v0

- **ContributionId:** `contrib:<random 128-bit base32>`, minted at random for one logical submitted intent under an
  authorized objective. Not a task ID, and not a promise that the contribution can later be
  removed from a synthesized descendant.
- **ContributionRevisionId:** a digest over the canonical record of one immutable revision:
  exact base and result `SourceSnapshotId`s, declared dependencies (as revision IDs),
  brief, and metadata. A metadata- or brief-only change is a new revision. Consumers pin a
  revision, never "latest". The record layout is #653's.
- **Scope:** identity and immutability are qualified here. Arbitrary subtraction from a
  synthesized descendant is not promised; composition lineage is E1's (#650).

### BrokerId and SessionRef: deferred

- **Design, unchanged:** BrokerId is minted at random per broker installation;
  SessionRef is (BrokerId, local integer session id). The integer alone is neither unique
  nor a permission, and broker session ids are already per repository.
- **Why deferred:** no portable record refers to a broker or session today, and the
  Coordination service that would need one is out of Local scope. Minting an identity no
  reader consumes would freeze a format without a consumer (D32). The per-repository UUID
  marker in `aethyme-broker::store::repository_identity` stays a local storage key. Reopen
  when the first cross-broker record needs to name its origin.

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
  build on them.

## D10 closure (confirmed 2026-11-03)

**Status: `accepted_with_evidence`**, confirmed by the maintainer on 2026-11-03 together with
the deterministic new-file FileId rule and the BrokerId/SessionRef deferral. Project authority binding, generic FileId
transitions and contribution revisions are implemented and qualified in the
[L1 identity evidence](local-v3-qualification/l1-identity-2026-11-03/README.md).

- **Evidence substitution:** the plan asks for canonical Rust/TypeScript fixtures. TypeScript
  parity stays deferred until a real TypeScript consumer exists (see T01). The Rust vectors
  plus the independent Python oracle (`aethyme-contracts/fixtures/experimental-v0/d10-independent/`)
  are the second implementation instead.
- **Limits:** experimental v0 formats, not frozen (D32). BrokerId/SessionRef deferred as
  above. Cross-project FileId correspondence is out of scope.
- **Reopen triggers:** a real TypeScript or Worker consumer; the first cross-broker record;
  a change to any identity preimage.

## Tests

| Plan test | State |
|---|---|
| T01 (canonical golden vectors) | Rust canonical vectors for source snapshots, records and analysis are present and independently checked. TypeScript parity is deferred until a real TS consumer exists; it is not evidence for unimplemented TS behavior. |
| T03 (two clones) | **Qualified.** Clones that explicitly bind one approved descriptor resolve identical FileId maps; a same-origin clone that enrolls on its own gets a different ProjectId and disjoint FileIds; the public CLI test covers independent enrollment, binding, association proposal, confirmation, reopen and revocation. See [L1 identity evidence](local-v3-qualification/l1-identity-2026-11-03/README.md). |
| T05 (rename, copy, split, competing paths) | **Qualified.** Contract vectors cover exact versus uncertain rename, edited rename, copy/split derivation, competing new files and renames, and foreign or wrong-base state. See [L1 identity evidence](local-v3-qualification/l1-identity-2026-11-03/README.md). |
| T65 (body edit keeps NodeId, revision changes) | Baseline done (#700). The "revision binds the exact input" half is #655's envelope. |
| T66 (scoped symbols, clone mapping) | The bounded AQ0 fixture is source-qualified: 4/4 actual-clone tests, including NodeId preservation, plus 3/3 symbol-vector tests. The adapter remains advisory and no producer integration is claimed. See [AQ0 evidence](local-v3-qualification/aq0-symbol-identity-2026-10-16/README.md). |
| T78 (conservative baseline degradation) | Timeout, omitted-language, truncation, unavailable-producer and stale-view cases retain explicit conservative outcomes in the independent analysis vectors; the final vector target passed 6/6. See [AQ0 evidence](local-v3-qualification/aq0-symbol-identity-2026-10-16/README.md). |
