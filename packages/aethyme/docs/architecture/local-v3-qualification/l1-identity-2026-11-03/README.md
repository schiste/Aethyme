# L1 identity qualification (#652), 2026-11-03

This qualifies plan tests **T03** (two clones) and **T05** (FileId transitions) for the
experimental v0 identities described in the [L1 identity record](../../local-v3-l1-identity.md).
Every attempt is kept, including the two that earn no credit. Machine-readable results are
in [`ledger.json`](ledger.json), and raw logs, commands and statuses are under `attempts/`.

## Source and environment

- **Qualified source:** commit `2f15b81a6630cab897d29ec43e3f9af2dd66c89a`, tree
  `01f756d43ab0fcf86fbd47877dc3a9d604682d2e`. `packages/aethyme/rust` was clean at that
  commit for every run; only documentation was uncommitted.
- **Host:** macOS (Darwin 27.2.0, arm64), Rust 1.96.0, `cargo --locked -j4`.

## What passed

| Plan test | Run | Before mutations | After restoration |
|---|---|---|---|
| T03, binding aligns FileIds; independent same-origin enrollment does not | `aethyme-broker` lib: `collaboration_cli::contribution`, `collaboration_cli::project`, `collaboration_identity_store` | 17 passed | 17 passed |
| T03, public CLI: independent enrollment, association propose/confirm/reopen/revoke | `aethyme-cli --test collab_project_association` | 1 passed | 1 passed |
| T05, rename/copy/split/competing paths, foreign and wrong-base state | `aethyme-contracts --test project_identity_vectors` | 9 passed | 9 passed |
| ContributionRevision immutability | `aethyme-contracts --test contribution_revision` (run 14) | 8 passed | n/a |

T03's two halves are separate tests. The positive half,
`two_real_clone_bindings_resolve_the_same_bootstrap_file_identity_map`, shows that a clone binding
the committed descriptor resolves an identical FileId map. The negative half,
`same_origin_clone_enrolled_independently_gets_disjoint_file_identities` (added by `2f15b81a6`),
gives a clone the same origin URL and anchor source, lets it enroll on its own, and shows that its
ProjectId and its FileId for the same path and bytes both differ.

## Mutation controls

Each control changed one predicate, ran one named test, and was then restored. `git diff` was
empty after each restore and at the end.

| Control | Diff | Test that failed (exit 101) |
|---|---|---|
| FileId preimage omits ProjectId | [diff](attempts/mutation-fileid-ignores-project.diff) | `same_origin_clone_enrolled_independently_gets_disjoint_file_identities` |
| Same change, contract vector | [diff](attempts/mutation-fileid-ignores-project-vector.diff) | `file_ids_are_project_scoped_and_repeatable_for_the_same_project_id` |
| An exact-content delete/add is claimed as an explicit rename | [diff](attempts/mutation-uncertain-rename-claimed-explicit.diff) | `exact_rename_preserves_id_but_unannotated_rename_stays_uncertain` |
| Competing-path conflict check disabled | [diff](attempts/mutation-competing-path-unchecked.diff) | `competing_new_files_at_the_same_path_conflict_even_with_equal_bytes` |

## Runs without credit

- **Run 05, scoped strict Clippy:** exit 101 on one pre-existing `collapsible_if` lint in
  `crates/aethyme-broker/src/cli/gc_storage.rs:1260`. That file is outside this package; local
  Clippy 1.96 is newer than CI's. No lint fired in any file this package changes. CI's lint job
  is authoritative.
- **Runs 03 and 12:** the `contribution_revision` filter on the contracts lib matched 0 tests,
  because those tests are an integration target. Run 14 is the real run.

## Limits

- Experimental v0 formats; nothing is frozen (D32).
- One host, macOS only. No Linux run was made for this package.
- The FileId correspondence that T03 qualifies is within one project, through one approved
  descriptor. Correspondence between separate projects is out of scope; it goes through
  `ProjectAssociation` and explicit symbol mapping records.
- BrokerId/SessionRef are deferred; see the identity record.
