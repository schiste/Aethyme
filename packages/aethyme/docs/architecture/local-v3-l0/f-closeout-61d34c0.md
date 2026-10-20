# Local v3 L0 — current-state closeout (#651)

Last Updated: 2026-10-20

## Revision and decision

This closes the **audit work package**, not the Local implementation or its
qualification. Plan §15.1 L0 (authoritative under §15.2) calls for a reuse,
cleanup, trust and analysis inventory at an actual source revision. The
[original audit](README.md) supplies the pre-implementation baseline at
`5e8daf712e26c63f1d4082e9a617f556cf995de2`; PR #704 merged that report.
Its 397 passing tests are historical evidence, not tests of today's tree.

This recheck is bound to source commit
`61d34c0294a8b9add6787bb17d74a5a89e4a6487`, tree
`9e276e3b560ba1af5a2d4fd47820f7c6e672f993`. The task worktree was clean before
this documentation change. Broker and graph source were checked independently.
Function and test names below are source references; their presence alone does
not establish execution. No product behavior, schema or gate policy is changed.

The repo-selected CLI and engine both report build revision `61d34c0`; the global
Cargo-installed CLI is older and was not used for this task. The maintained
repository uses `verify-only`, authorizes session-branch pushes, has graph
authority disabled, and sets collaboration capture to `advisory`. Normal broker dogfooding is authorized here; destructive
cleanup and analysis evaluations remain confined to disposable Playgrounds.

**Disposition:** accept the six L0 checklist items and D37's inventory with the
limits below. Open implementation or qualification work remains with its owning
package; an issue's closed status is not proof of an implemented guarantee.
See the [qualification ledger](../local-v3-qualification.md) for later evidence.

## Broker, trust, cleanup and compatibility recheck

Paths in this section are relative to
[`aethyme-broker/src`](../../../rust/crates/aethyme-broker/src/).

| Seam | Status at this revision | Evidence and boundary |
|---|---|---|
| Enrollment, start, leases, submit, finish, hooks, JSON and exit codes | **Changed** since slice E | Legacy fixtures remain in `tests/json_shape_snapshots.rs`, `tests/finish_cli.rs`, `tests/merge_e2e.rs` and CLI help snapshots. A current gate-policy refusal under `--json` returned `aethyme.broker-error/v1` with code 3, unlike slice E’s historical empty stdout. Slice E is not a current universal error/exit contract. |
| Submit and composition | **Changed** | `collaboration_submit::submit` keeps capture-off on the legacy route and binds required capture to `submit_expecting_head`; `collaboration_submit::require_capture_for_promotion` fences uncaptured heads. `composition.rs` and `composer.rs` now define a typed candidate and composer. Their existence does not prove the ordinary submit route uses multi-contribution composition. |
| Policy trust | **Confirmed**, scoped | `merge::require_trusted_policy_at_commit` and `broker::gate_trust::policy_at_commit` retain base-pinned policy and human trust. `gate_trust_cli` covers a new clone and a subsequently landed policy change. |
| Gate invocation and evidence | **Changed**, scoped | Ordinary gates still use `gates::run_gate_command` / `spawn_gate_command`. The separate `execution_supervisor::DockerExecutor` is not evidence that ordinary gates run there. `gate_profile::ExecutionProfile::capture` now feeds profile-bound cache lookup. Test `execution_profile_cache_requires_definition_and_profile_and_names_its_source_run` covers definition/profile/source-run binding. |
| Collaboration state and retention | **Changed** | `collaboration_state::CollaborationRoot::resolve` and `CollaborationStore::open` use a private marked root and separate `state.db` (schema 30). `collaboration_gc::plan/apply/resume` intentionally reclaim there under their own ownership rules. The old statement that no collaboration code exists is historical. |
| Legacy cleanup reach | **Confirmed only within recorded scope** | `tests/collaboration_state_cleanup.rs` exercises legacy deleters with retained collaboration bytes present, including inline sweep, GC, storage and finish paths. This is stronger than the original unactioned T35 proposal; it is not an execution of every old released binary against current state. |
| Startup and hooks | **Changed / residual documentation gap** | Full broker opens resume GC maintenance, including disposable-checkout removal. `PreToolUse` uses `Broker::open_snapshot`; `PostToolUse` only fully opens when unread notes require it. Other registered lifecycle hooks can fully open. Thus cleanup is not unconditional on every tool call. The `resume_gc_maintenance` cache-only comment remains too narrow; retain this as a #335 maintenance follow-up despite #698's closed status. |
| Publication | **Confirmed**, scoped | `session_push::push_session` requires branch publication policy. `ship` refuses verify-only; tests include `ship_refuses_verify_only_repositories_and_points_to_pull_request_delivery` and `pull_request_delivery_distinguishes_merged_from_target_verified_publication`. A merged PR is not automatically target-verified publication. |
| Worktree root ownership | **Unverified for the complete guarantee** | A source inventory and closed #697 do not qualify durable root identity across environment changes, remounts and every cleanup path. Keep that boundary explicit; no root migration or destructive experiment was performed for L0. |

### D18: disabled and mixed-client matrix

| Situation | Current seam / evidence | Limit |
|---|---|---|
| Capture off or absent | `collaboration_submit.rs`; CLI `collab_cli::without_a_policy_status_is_disabled_and_everything_else_refuses` | Disabled collaboration creates no collaboration directory in that fixture; this is not a new replay of every historical CLI contract. |
| Advisory vs. required | `an_advisory_capture_retains_the_commit_that_was_submitted`, `a_required_capture_submits_the_captured_commit`, `a_session_moved_after_a_required_capture_is_not_submitted`, `an_uncaptured_head_is_never_promoted_under_required` | T34 has executable coverage now; the original “not runnable” wording applies only to October 9. |
| Unknown collaboration keys | `an_unknown_setting_is_refused_only_under_required` | Strict under required, warning/ignored under advisory; do not generalize this rule to all config sections. |
| Old binary on current broker.db | `schema.rs`: schema 52, minimum compatible schema 52; `v52_adds_nullable_gate_profiles_and_fences_older_cache_readers` | The earlier additive marker/floor 47 description no longer permits an old binary on this database. No downgrade was attempted. |
| Legacy cleanup beside retained state | `collaboration_state_cleanup::retained_state_survives_every_legacy_deleter` and its external/overlapping-container cases | T35 coverage uses the current implementation's legacy deleters and fixture-owned storage, not an exhaustive mixed-release matrix. |
| Generic Git / optional semantic producer | Existing `ship` / `session_push`; analysis reports unavailable or partial data | Provider integration and absent semantic producers are not inferred from Local code. Partial/shallow-clone behavior remains unqualified by this audit. |

**T33–T35 disposition:** L0's baseline and test mapping are complete. Current T33
fixtures include disabled CLI behavior; T34 has capture/fence regressions; T35 has
legacy-cleaner isolation tests. Full disabled-contract and mixed-client
qualification stays with L2/L9 (#660/#681/#682). L0 does not mark those plans PASS.

## D37: analysis inventory accepted with explicit limits

Paths below are under [`rust/crates`](../../../rust/crates/).

| Area | Rechecked source and tests | Disposition |
|---|---|---|
| Typed provenance | `aethyme-graph-schema/src/edge_struct.rs`: edges carry `Confidence` and method `Source`; nodes carry neither. Tests `source_serde_round_trip`, `confidence_deserialize_validates_range` | **Confirmed.** Method provenance is not producer/profile/snapshot provenance. |
| Deterministic fragments | `aethyme-graph-storage/src/fragment.rs`: sorted bincode, schema 1. `fragment_binary` tests include `same_inputs_produce_byte_identical_fragments` and `different_input_order_produces_same_bytes` | **Confirmed.** No per-fragment content digest or producer/profile stamp. |
| Overlay reads | `aethyme-graph-storage/src/overlay.rs::read_overlay_bytes`; `overlay_byte_reader_requires_the_expected_producer_version`, `overlay_byte_reader_rejects_an_empty_version_expectation` | **Changed.** Producer version is now checked; historical #699 gap is resolved at this seam. |
| Cache / exact-source manifest | `aethyme-graph-storage/src/cache.rs::GraphStoreCacheKey`, `manifest.rs::GraphAuthorityManifest` / `committed_source_tree_digest`, CLI `graph_refresh::validate_fragment_authority`; `key_digest_binds_every_compatibility_dimension`, `materialize_rebuilds_only_the_local_store_from_verified_committed_fragments`, `materialize_refuses_exact_head_drift_without_cloning_or_indexing` | **Confirmed**, limited. Key binds source/fragments, engine, protocol and storage schema, not producer configuration. Source digest hashes Git tree/object records, not the raw-byte `SourceSnapshotId` preimage. |
| NodeId / FileId | `aethyme-graph-schema/src/identity.rs::NodeId::new`: length-prefixed repository/path/name/kind; file content hash stored separately | **Confirmed.** No separate `FileId`; body independence follows from inputs but lacks a dedicated test (#700 remains open). Do not silently replace this identity with Local source identity. |
| Session refresh / library writes | CLI `graph_refresh::broker_preconditions`; storage `write_guard::ensure_isolated_graph_write`; `active_sessions_block_shared_fragment_writes`, `raw_shards_coverage_and_manifest_refuse_before_side_effects` | **Changed.** Raw writes are now denied in broker-managed repositories. This is a deny-only storage fence; session/lease awareness remains in the CLI, not a shared lower-level session guard. |
| Linker / producers | `aethyme-graph-indexer/src/linker.rs::unique_call_target`; `ambiguous_name_only_match_stays_unresolved`, `rust_ambiguous_call_fails_closed_rather_than_guessing` | **Confirmed.** Conservative unique compatible match; no full type/name/cross-language resolution. Rust/TypeScript Calls exist; no SCIP implementation is established. |
| Edge conversion / schemas | `aethyme-engine/src/map.rs::schema_edge_kind_to_model`; `schema_tests_relationships_remain_distinct_in_the_runtime_model`, `schema_v9_store_is_rejected_after_test_edge_kind_addition` | **Changed.** Tests edges are preserved; redb is schema 10 and rejects v9. Fragment/overlay/cache/manifest wire schemas stay at 1; exact engine pin remains necessary. |

This inventories guarantees rather than adopting the old I3 assessment as proof.
D37 is complete as an audit decision. NodeId body-independence coverage (#700),
producer/profile provenance and refresh/impact limits remain with #655/#684/#685
and their Local owners. Emitted-edge status feeds #213. None is a new guarantee.

## Current reuse map for downstream Local work

These are **changed** reuse seams since the original baseline, not claims that
every operational path is wired or qualified. The X1–X4 row explicitly retains
unverified capabilities. All broker modules are in the source directory linked above.

| Package | Current reuse seam | Remaining boundary |
|---|---|---|
| L1 identity / records / brief / analysis (#652–#655) | `aethyme-contracts/src/experimental_v0` identity, record, brief and analysis contracts; `collaboration_identity_store`; existing graph contracts | Preserve identity wire formats and exact-source binding; no assumed FileId or graph producer provenance. |
| L2 state / archive / capture / GC / opt-in (#656–#660) | `collaboration_state`, `collaboration_archive`, `collaboration_capture`, `collaboration_gc`, `collaboration_submit` | Separate state ownership, retention and old-client fences must stay enforced; no global mixed-client claim. |
| L3 context / invalidation (#661–#662) | `collaboration_context` and its revision cache | Fresh/private views and exact subjects remain distinct from a shared indexed HEAD. |
| L4 candidate / composer / resolution (#663–#665) | `composition`, `composer`, `resolution`, typed verification candidate | A composer profile's bounded evidence does not qualify every submit/gate integration or structural case. |
| L5 target / wave / controller / retry (#666–#669) | `local_target`, `local_wave`, `local_controller` | Controller/model tests and appointed service evidence are separate from ordinary broker submit. |
| L6 snapshot / execution / verifier / resources (#670–#673) | `execution_snapshot`, `execution_supervisor`, `execution_verifier`, `execution_resources` | Preserve independent evidence/profile binding; separate execution adapters are not automatically the ordinary gate runner. |
| L7 publication / confirmation (#674–#675) | `local_controller::bare_destination`, `session_push`, `ship`, coordinated operations | Keep exact destination confirmation and merged-vs-published distinctions; provider guarantees are not inferred. |
| L8 generation / handoff / adoption (#676–#679) | `local_generation`, `local_handoff`, shared `outbox` | Persisted proof and safe cleanup remain required at every ownership transition. |
| L9 CLI / compatibility / QA (#680–#682) | `collaboration_cli`, CLI `collab_cli` fixtures, help/JSON/error snapshots, schema floor | Full final-tree Local qualification remains separate from L0 acceptance. |
| Analysis extensions X1–X4 (#683–#686) | Existing linker, private views, graph impact and structured analysis contracts | No assumed SCIP, producer identity or usefulness guarantee. |

## Evidence and remaining follow-ups

The October 9 transcript and 397-test summary remain in slices A–E and README §7;
this task does not manufacture replacement raw captures. Current source references
above were rechecked at the pinned revision. Local focused validation passed all
29 graph identity and 12 fragment tests. The first docs hygiene run found an
anchored-link checker incompatibility (13 passed, one failed); the link was
changed to the document target and all 14 docs tests passed on rerun. Any later
review edits, final gate results and delivery are recorded separately in the PR
test plan; source references are not execution claims.
No destructive storage operation, model trial or held-out evaluation is needed
for this documentation-only closeout.

At the time of recheck, #688 and #700 remain open. `git.rs::GitRepo::discover` now
preserves timeout/spawn/unrelated Git errors rather than calling them all
`NotARepository` (tests `discovery_preserves_an_expired_deadline_instead_of_denying_the_repository`
and `discovery_still_identifies_an_actual_non_repository`); the open #688 is not
evidence that this old defect remains.
 Follow-ups #689–#699 and #701–#703 are closed on GitHub; source rechecks establish the particular
cache, overlay, write-fence and edge-conversion deltas above, not all guarantees
suggested by their titles. The stale inline-sweep comment and durable root
qualification remain explicitly bounded even where an issue was closed. The
[original follow-up register](README.md)
retains ownership links, including broker #335 and analysis #213.

No duplicate store, engine or cloud dependency is introduced. Accepting #651
unblocks use of the inventory; it does not accept unverified Local behavior.
