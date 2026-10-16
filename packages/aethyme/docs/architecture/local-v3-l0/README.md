# Local v3 — L0 baseline audit (#651)

Last Updated: 2026-10-09

This is the revision-bound reuse and compatibility map that plan v3 requires before
Local adds collaboration state or analysis contracts (plan §2.3, §6.1, §6.10, §15.1 L0;
decisions D18 and D37; tests T33–T35). It records what the code does **today**. It does
not change behavior, and it is not proof that any later Local capability works.

| | |
|---|---|
| Baseline commit | `5e8daf712e26c63f1d4082e9a617f556cf995de2` (tree `3cf78bd42b1e1757ac21849039096921f54b0736`), `origin/main` on 2026-10-09 |
| Binary used for live observations | `aethyme 0.8.26 (build_commit=5e8daf71, build_date=2026-10-08T19:30:03Z)` |
| Repository policy at baseline | `[promote] mode = "verify-only"`; `[delivery] push_session_branches = true`; `[graph] authority = "disabled"` (`.aethyme/config.toml`) |
| Host | macOS (Darwin 27.2), worktree container from `AETHYME_WORKTREE_ROOT` on an external volume |
| Method | Five read-only slices (below), each citing `file:function:line` and the tests that enforce a claim. The synthesizer then re-checked the claims marked † in this document against code or by execution, and ran the targeted tests listed in §7. |

**Status key** (used in every slice): **confirmed** means the code at the baseline matches
the claim. **changed** means the code differs from the plan, the I3 assessment, or the issue
wording. **unverified** means the claim was not established. Nothing marked unverified may be
cited later as a guarantee.

**Graph caveat:** the graph is disabled for this repository, so every analysis observation
below comes from reading code and tests, never from a live graph of Aethyme. Live analysis
qualification belongs on Playground repositories (Cardinal Rule 1).

## Evidence slices

| Slice | File | Scope |
|---|---|---|
| A | [a-submit-publication.md](a-submit-publication.md) | Submit, replay, candidate, promotion, `ship`, session push, coordinated operations, outbox, review dispatch, landing proof |
| B | [b-gate-trust-execution.md](b-gate-trust-execution.md) | Gate config loading and trust, selection, materialization, process supervision, result binding, cache, host resources |
| C | [c-state-cleanup-reach.md](c-state-cleanup-reach.md) | Every persistent root, every deleter and its reach, where new state can safely live (D09/T35), hooks |
| D | [d-analysis-inventory-d37.md](d-analysis-inventory-d37.md) | D37: schema, fragments, overlays, redb, manifests and caches, session guard, NodeId, linker, emitted edges, `graph_impact`, SCIP |
| E | [e-t33-cli-baseline.md](e-t33-cli-baseline.md) | T33: which CLI contracts are pinned by tests, plus a live JSON/exit-code capture on a disposable repository |

## 1. Audit checklist (#651)

| Item | Result |
|---|---|
| Record the exact revision and the promotion and publication policy | Done (table above). |
| Freeze observable legacy behavior (start, submit, finish, gates, ship, cleanup, hooks, JSON, exit codes) | Done for the default no-config path on a disposable repository (slice E §2). Pinned vs. unpinned surfaces are listed in slice E §1. Behavior under this repository's `verify-only` policy comes from code reading (slice A §2). |
| Trace gate trust from configuration through scripts, helpers, environment and invocation | Done (slice B). Several trust gaps are listed in §2.1 below. |
| Inventory state stores, retention and cleanup roots, deletion ownership, failure behavior | Done (slice C), by reading code. No destructive experiment was run on this checkout. |
| D37 analysis inventory | Done (slice D). The proposed D37 closure is in §4. |
| Name a reuse seam or a gap for each L1–L9 and AQ0 capability | Done (§3). |

## 2. Headline findings

These change the shape or order of later work. Each one links to its evidence.

### 2.1 Gate evidence is not yet independent enough for L6 (slice B)

- **What holds:** gate *policy* is pinned to the base commit and never read from the candidate (`merge.rs:1035–1080`), and nothing repository-defined runs before a human approves the policy digest (`gate_trust.rs`).
- **What L6 must add:** today the gate child is not confined (environment, filesystem reach, resources), the verdict path is not independent of candidate-controlled code, storage and output, and cache reuse is not bound to a full execution profile or isolated between candidates. These are tracked at design level in #671, #672 and #673; following `SECURITY.md`, exploitable detail is not published here.
- **Public, non-sensitive gaps:** the result cache key is `(gate, tree hash, definition hash)` with no test that a definition change misses it (#689), and one gate installs an unpinned dependency (#690).
- **Reusable as-is:** the exact-commit disposable verification slot (`verification.rs`); process groups, pidfiles with start time, and SIGTERM→SIGKILL timeouts; the disk and inode floor; load admission; atomic resource leases.

### 2.2 There is a safe home for collaboration state, but only one (slice C)

- **`<host-state>/collaboration/<project-id>/`** (by default `~/Library/Application Support/Aethyme/collaboration/`) is the only candidate root that no current deleter enumerates. Every other consumer of host state joins its own fixed subpath. **Every other candidate is unsafe:** the worktree container (orphan sweep), session worktrees, the host cache, and the repository's `.aethyme/` (lost with the clone, which §6.3 rules out). T35 must still prove this by running `gc plan`/`gc apply`, storage apply and the sweep with the directory present.
- **Use a new `state.db`; never extend the host databases.** `host-operations.db` requires an exact schema version, so any change locks every older binary on the host out of it. `broker.db` is v49 with `MIN_COMPATIBLE_SCHEMA = 47` (`schema.rs:36,68`), so additive tables are tolerated but a non-compatible bump locks old binaries out.
- **The worktree container depends on the environment, and this host has two.** A shell or hook without `AETHYME_WORKTREE_ROOT` inventories a different container. `storage_container` also ignores the committed `[worktrees] root` that session start honours. Collaboration state must not derive its location from that variable.
- † **Any broker command can remove a whole closed checkout.** The inline sweep that runs on every broker open, including plugin hook calls on every tool call, invokes `auto_remove_disposable_checkouts` (`gc.rs:2255–2262`). The comment at `gc.rs:2103–2109` says it only removes build caches, which is stale. The four landing proofs still apply. This is now part of the recorded legacy baseline.
- **The repository key is derived from the repository's path.** That is fine for local storage, but it must not become #652's `ProjectId`.

### 2.3 No combined candidate exists today, and its identity is untyped (slice A)

- In `verify-only` mode, each session is replayed and gated **alone** against `origin/main`. No multi-session candidate is ever built or gated. Interaction shows up only as a textual conflict after the other work has landed.
- Composition is a per-commit textual `git merge-tree` over a linear pending suffix; merge commits are refused. The only lineage logic is skipping inherited and already-integrated commits (by ancestry or patch-id), which is the nearest analogue of FX03.
- **Seams:** plan and replay are pure and separable, and `promote` is a separate compare-and-swap. The candidate commit is minted inside `simulate_and_gate_against`, and its identity lives only in an untyped `details_json`. #663 needs a typed candidate record and has to split minting out of gating.
- † **An unknown `[promote] mode` falls back to `Auto`** (`merge.rs:180–189`), deliberately, so that a typo does not break the broker. That conflicts with plan §6.7 ("refuse critical unknown settings"); #680 must decide.
- **The delivery outbox is still PR-typed** despite its "provider-neutral" doc, and `repo_watch.rs` holds a second copy of the mechanics. Extract the claim/generation/backoff/dead-letter mechanics before #676 adds a third copy.
- **Reconciling an `outcome_unknown` operation is operator-asserted**, with no re-inspection of the remote. Retry is correctly blocked, but #674's "inspect before retry" is still a manual step.
- **Two landing proofs coexist:** content/patch-equivalence in `representation::work_landed`, and stable cumulative patch-id in integration reconcile. They can disagree on the same head (#335 Q2).

### 2.4 The analysis engine can host an envelope, but has no revision-bound subject (slice D)

- **Confirmed per I3:** typed schema (edges carry `Confidence` and `Source`; nodes carry neither), deterministic per-file fragments, canonical `_overlays`, the derived redb store (v9, rebuilt rather than migrated), the exact-source manifest and host-cache key, the engine-version pin, and the live-session refresh guard.
- † **`NodeId`** is BLAKE3 over length-prefixed (repo, path, name, kind), 128 bits, base32 (`identity.rs:108`, `:234`). It does not depend on the function body. No test asserts that (the T65 precondition). There is **no FileId**, and two ID spaces coexist: schema NodeIds, and the engine's path-string IDs (`file:`, `dir:`, `doc:`, …).
- **Gaps:**
  - The overlay `producer_version` is written but never checked on read (`overlay.rs:155`).
  - The session guard lives only in the CLI layer (`aethyme-cli/src/graph_refresh.rs:802`), so library writes bypass it.
  - The source digest hashes Git object ids, not raw bytes, so it differs from #652's `SourceSnapshotId` preimage.
  - † `Tests`, `Implements` and seven other kinds collapse to `References` (`map.rs:1287–1296`), so newly emitted `Tests` edges reach `graph_impact` lossily.
  - `graph_impact` can describe only the committed, indexed HEAD: not a worktree, not a session branch at another commit, and not a synthesized candidate.
- **#213's status table is stale.** † Rust and TypeScript `Calls` producers exist (`rust_calls.rs`, `typescript_calls.rs`), without Rust method calls. PHP has no `Calls`. Non-code `Documents`, `References` and `Tests` edges landed on 2026-10-08.
- **No SCIP code exists, and there are no Rust SCIP notes;** X1 starts from zero.

### 2.5 The CLI has no machine-readable error contract (slice E)

- Five `--json` success shapes are pinned (start, adopt, submit, status, finish), and help text is pinned for every surface. **Nothing pins refusal output or exit codes.**
- Under `--json`, every refusal prints empty stdout, a plain `Error:` line on stderr, and exits 1, 2 or 3. **A blocked `finish` exits 0** with `closed:false`.
- With no configuration, promotion defaults to `auto`, so a test fixture's default differs from this repository's `verify-only`.
- No network access and no lingering process or daemon were observed.
- **#688 diagnosed:** `GitRepo::discover` maps every git error to `NotARepository`. † Confirmed live: `finish` fails at the 10-second default deadline and succeeds with `--timeout 120` (recorded on #688).

## 3. Reuse map for each Local package

"Reuse" names the existing seam to build on. "Gap" is work that does not exist yet; it is a
scoped follow-up, not something fixed here.

| Package | Reuse | Gap |
|---|---|---|
| **L1-ID #652** | `NodeId` (keep its wire format); `committed_source_tree_digest` mechanics; the repository key as a local storage key only | No FileId; two ID spaces; path-derived repository key; no raw-byte snapshot digest; no body-independence test |
| **L1-RECORD #653** | Events contract v1 (`contract_v1.rs`) as the pattern for frozen versioned records; `broker.db` compatible-migration floor | No typed candidate or contribution record; artifacts with a "forever 1" format must be wrapped, not bumped |
| **L1-BRIEF #654** | — | Nothing exists |
| **L1-ANALYSIS #655 / AQ0** | `GraphCoverage`, `Freshness`, `GraphImpactContractStatus`, `GraphImpactLimits`, `GraphImpactProvenance`, `UnresolvedSymbol` | No producer or profile identity; no `incompatible` status; truncation is a flag, not a status; `producer_version` unchecked |
| **L2-STATE #656** | Host-state layout and permissions (`host_state.rs`, 0700/0600) | Safe root is `<host-state>/collaboration/` (§2.2); separate `state.db`; T35 proof outstanding |
| **L2-CAS #657** | `recovery-archives` precedent for host-level retained bytes | No content-addressed source archive |
| **L2-CAP #658** | Coordinated-operation journal (intent → outcome, `outcome_unknown`) | No capture state machine |
| **L2-GC #659** | Digest-confirmed GC plan/apply with revalidation; the four-proof auto-removal | Collaboration retention classes; keep it out of the inline sweep's reach |
| **L2-LEGACY #660** | Pinned JSON shapes (slice E §1); `PromotionIntent` narrows a run without changing policy | No JSON error envelope; blocked `finish` exits 0; unpinned surfaces |
| **L3 #661 / #662** | `graph_impact` modes and contract statuses; `UnresolvedSymbol` for "unknown, not absent" | Cannot query worktrees or candidates; `Tests` edges are lossy |
| **L4-BOUNDARY #663** | `build_submission_plan`, `replay_submission_plan`, `promote` (CAS) | Candidate minting is fused with gating; identity only in `details_json` |
| **L4-COMPOSER/RESOLVE #664 / #665** | Inherited-commit skipping (ownership × integration state) | No multi-contribution or structural composition |
| **L5 #666–#669** | Promotion CAS plus re-queue; outbox claim and generation fencing; host resource leases with TTL | No controller identity, fairness, per-target budget, or retry lineage; admission is a wait, not a reservation |
| **L6-SNAPSHOT #670** | `ExactTreeVerificationSlot` (detached checkout of the exact commit) | No execution identity covering toolchain, environment, filters, submodules or generated inputs |
| **L6-EXEC #671** | Process group, pidfile, timeout escalation, lease-loss kill, debris labels | Confinement of the gate child (see #671) |
| **L6-VERIFIER #672** | Base-pinned policy; host-state trust record | Evidence independent of candidate-controlled code and storage; profile binding; expiry and revocation (see #672) |
| **L6-RESOURCES #673** | Disk floor, load admission, managed-cache rotation inside a lease | A profile-aware cache key (#689); build-cache isolation between candidates (see #673); pinned fetches (#690) |
| **L7 #674 / #675** | Exact-SHA confirmation plus plan digest (`ship`); `PullRequestMerged` vs. `Published`; `session_push` (the verify-only route) | Machine-checked reconcile; Mergify mapping unverified; L7 must cover both `ship` and `session_push` |
| **L8 #676–#679** | Outbox claim/generation/backoff/dead-letter mechanics; `work_landed` | Mechanics duplicated in two PR-typed outboxes; no generation or handoff record |
| **L9 #680 / #681** | Help and JSON-shape snapshots; `MIN_COMPATIBLE_SCHEMA` floor | Unknown promote mode falls back to `Auto`; no exit-code contract |
| **X1 #683** | `scip-mining-notes.md` (Python and TypeScript only) | Everything; no Rust SCIP knowledge recorded |
| **X2 #684** | Canonical `_overlays` namespace to stay out of; CLI session guard | A private-view root outside `_overlays/`, and a guard in the library layer |
| **X3 #685** | `graph_impact` contract | Candidate subjects; lossless `Tests` edges |

## 4. D37 — proposed closure record

D37 asks which engine guarantees are actually implemented at the development revision.
Slice D answers each I3 claim with file, function and tests:

- **Confirmed:** typed schema with edge-level confidence and method provenance; deterministic fragments; canonical overlays; derived redb v9 (rebuild, never migrate); exact-source manifest; host-cache key; engine-version pin; live-session refresh guard (CLI layer); NodeId formula; conservative unique-match linker.
- **Changed:** emitted edge kinds (Rust and TypeScript `Calls` and non-code `Documents`/`References`/`Tests` exist; #213 is stale); catch-all collapse moved to `map.rs:1287–1296`; the source digest is over Git object ids.
- **Unverified or absent:** enforcement of overlay `producer_version`; a body-independence test for NodeId; any SCIP code; a session guard below the CLI layer.

Proposed D37 status: **ready to close** with this record, if the plan owner accepts the
changed and unverified items as explicit work (listed in §6) rather than guarantees.

## 5. D18 and the compatibility baseline (plan §6.8)

| §6.8 situation | Baseline behavior today | Evidence |
|---|---|---|
| Collaboration disabled | No collaboration code exists, so legacy behavior is the baseline captured in slice E | E §2 |
| Old binary on the same repository | `broker.db` additive changes ≥ v47 are tolerated, a non-compatible bump locks it out; an unknown directory in host state is ignored; an unknown unmarked directory in the container is reported, never removed | C §3 |
| Old cleanup reaching new data | Cannot reach `<host-state>/collaboration/`; can reach anything inside the container or a session worktree | C §2–§3 |
| GitHub squash, rebase or merge | Content-based landing proof in `work_landed`; integration reconcile uses a different algorithm | A §7 |
| Generic Git without a review API | `ship` in direct mode, or `session_push` limited to `agent/*` | A §3 |
| Partial or shallow clone | Not audited in depth; cleanup's shallow-clone diagnostic was fixed in #525. Composition is not applicable yet | unverified |
| Optional semantic producer absent | No SCIP producer; `graph_impact` reports `Unavailable` or `Partial` rather than an empty result | D rows 10–11 |
| Coordination or native acceptance unavailable | Not applicable; no such components exist | — |

## 6. T33–T35 mapping

| Test | Status at L0 | Where it can actually run |
|---|---|---|
| **T33** old CLI/JSON/exit-code fixtures with the feature disabled | **Baseline captured** (slice E). That records the legacy side; it is not a pass. | L2-LEGACY #660 and L9 #680/#681, against the slice E table, once a new feature exists to disable |
| **T34** advisory vs. required capture failure | **Not runnable**: no capture code exists. L0 records the legacy outcomes it must leave intact. | L2-CAP #658 and L2-LEGACY #660 |
| **T35** old and new clients plus cleanup on an opted-in test clone | **Not runnable.** L0 names the root that should pass (`<host-state>/collaboration/`) and the deleters T35 must exercise: inline sweep, GC plan/apply, storage apply, orphan sweep, finish cleanup. | L2-STATE #656 and L9-COMPAT #681 |

## 7. Targeted existing tests run

Run on 2026-10-09 in the session worktree at the baseline commit, with a clean `PATH` (no wrapper shims):

```sh
cargo nextest run --manifest-path packages/aethyme/rust/Cargo.toml --locked --no-fail-fast \
  -p aethyme-broker -p aethyme-cli -p aethyme-graph-schema -p aethyme-graph-storage \
  -E 'binary(merge_e2e) | binary(gate_trust_cli) | binary(gate_database_isolation) | binary(promote_mode) | binary(auto_cleanup) | binary(gc_sweep_cli) | binary(ship_e2e) | binary(operations_e2e) | binary(pr_watch) | binary(finish_cli) | binary(json_shape_snapshots) | binary(graph_refresh_cli) | binary(help_snapshots) | binary(identity) | binary(fragment_binary) | (package(aethyme-broker) & kind(lib) & (test(/^representation::/) | test(/^graph_impact::/) | test(/^schema::/) | test(/^verification::/)))'
```

**Result: 397 run, 397 passed, 0 failed** (9 slow; 642.6 s; 1,212 tests outside the selection skipped).

| Binary | Passed | Backs |
|---|---|---|
| `aethyme-broker` lib: `schema::` / `representation::` / `graph_impact::` / `verification::` | 44 / 21 / 11 / 11 | broker.db compatibility floor (§2.2); landing proof (§2.3); impact contract (§2.4); exact-commit slot (§2.1) |
| `merge_e2e` | 57 | Submit, replay, base-pinned gate policy, promotion CAS (§2.3) |
| `operations_e2e` | 47 | Coordinated operations and `outcome_unknown` (§2.3) |
| `gc_sweep_cli` / `auto_cleanup` | 42 / 28 | Deleter reach, orphan sweep, four-proof auto-removal (§2.2) |
| `ship_e2e` / `pr_watch` | 41 / 7 | Publication, verify-only refusal, outbox fencing (§2.3) |
| `identity` (graph-schema) / `fragment_binary` (graph-storage) | 29 / 12 | NodeId formula and fragment determinism (§2.4) |
| `graph_refresh_cli` | 20 | Engine pin, exact-source materialization, live-session guard (§2.4) |
| `finish_cli` / `promote_mode` | 9 / 7 | Finish semantics, promote modes (§2.5, §2.3) |
| `gate_trust_cli` / `gate_database_isolation` | 5 / 4 | Trust-on-first-use, per-run DB isolation (§2.1) |
| `json_shape_snapshots` / `help_snapshots` | 1 / 1 | Pinned CLI shapes and help (§2.5) |

These tests confirm the **existing** behaviour the audit relies on. They do not test any gap listed in §8; those have no tests by definition.

## 8. Scoped follow-ups (recorded, not fixed here)

| # | Follow-up | Feeds | Tracked in |
|---|---|---|---|
| 1 | Confine the gate child process | #671 | design-level note on #671 |
| 2 | Make trust decisions on the run path visible in, and binding on, gate evidence | #672 | design-level note on #672 |
| 3 | Make the verdict path independent of candidate-controlled code, storage and output | #672 | design-level note on #672 |
| 4 | Add execution-profile identity to the gate cache key; add a test that a definition change misses the cache | #670, #673 | #689 |
| 5 | Isolate build caches between candidates, or bind their state into the result | #673 | design-level note on #673 |
| 6 | Pin `pytest` in `pytest-aethyme-eval` | #673 | #690 |
| 7 | Build verification tooling independently of the candidate under test | #672 | design-level note on #672 |
| 8 | Decide how an unknown `[promote] mode` is handled (refuse vs. fall back) | #680 | #691 |
| 9 | Add a typed candidate record and split minting from gating | #663, #653 | #692 |
| 10 | Add a parity test for the verify-only `submit plan` vs. `submit` base, and fix the stale comment at `merge.rs:637` | #663 | #693 |
| 11 | Extract shared outbox mechanics before a third copy | #676 | #694 |
| 12 | Machine-checked reconcile for exact-ref pushes | #674 | #695 |
| 13 | One landing proof for finish, reconcile and cleanup | #335 | #696 |
| 14 | Record the resolved worktree container per repository; make `storage_container` honour `[worktrees] root` | #335, #656 | #697 |
| 15 | Fix the stale comment at `gc.rs:2103–2109`, which says the inline sweep only removes caches | #335 | #698 |
| 16 | Check overlay `producer_version` on read | #655 | #699 |
| 17 | Add a NodeId body-independence test | #652 | #700 |
| 18 | Move the refresh session guard below the CLI layer, or route X2 through it | #684 | #701 |
| 19 | Stop collapsing `Tests` edges to `References` | #213, #685 | #702 |
| 20 | Update #213's tranche status | #213 | comment on #213 |
| 21 | JSON error envelope and an exit-code contract; make a blocked `finish` non-zero | #660, #680 | #703 |
| 22 | Make `GitRepo::discover` keep its cause | #688 | #688 |
