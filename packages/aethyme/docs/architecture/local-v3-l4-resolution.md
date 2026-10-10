# Local v3 — L4 resolution requests and candidate manifests (#665)

Last Updated: 2026-10-10

This record covers plan §7.3 steps 6–9, §7.4 and §7.5 on top of the provisional composer
(#664, `local-v3-l4-composer.md`). It is **provisional**. E1 (#650) has not run, so D06
(exclusion from synthesized results), D08 and D12 stay open. No resolver ships: the
resolver is a narrow interface, exercised by test doubles. D15 sets real token, time and
byte caps before any paid dispatch.

Code: `aethyme-broker::resolution`, plus:
- crate-private helpers in `composer` (`snapshot_entries`, `blob`, `materialize_candidate`,
  `retain_commit`);
- a `Synthesized` mode and a `Resolver` producer in `composition`;
- the `unicode-normalization` dependency (already in the lockfile) for path folding.

No gate, queue, submit or broker.db change. Nothing calls this outside tests yet.

An independent review of the first version found six defects (F1–F6, below). Each has a
regression test in `tests/resolution_boundaries.rs`.

## Decided here

### Three records, all in the archive

All three are #653 canonical records, written as archive objects and read back with
`read_record`, which refuses bytes that do not decode to the record's ID.

| Record | Schema | Written by |
|---|---|---|
| Candidate manifest | `aethyme.candidate-manifest/experimental-v0` | `write_manifest` |
| Resolution request | `aethyme.resolution-request/experimental-v0` | `request_resolution` |
| Synthesized group | `aethyme.synthesized-group/experimental-v0` | `resolve` |

**Candidate manifest.** One per composition, whatever its outcome.

- It first validates the composition against its request: the order names only known
  contributions; the baseline matches; and a candidate's recipe matches its subject,
  commit, baseline and profile.
- The profile is the candidate's recipe profile. An outcome without a candidate has no
  recipe, so it records the composer's one provisional profile.
- The baseline snapshot is recorded. If it cannot be read, the reason is recorded instead
  (`baseline_unreadable`); it is never silently dropped.
- Only a candidate's manifest has a subject, commit and mode, and only it awaits
  verification.

**Resolution request.** Raising one is the authorization point: any preference it carries
was given by whoever raised it, and cannot be added afterwards. It is raised from:

- **A conflict.**
  - The **members** the resolver decides between are the contributions applied up to and
    including the conflicting step that touch a conflicting path.
  - The **contents** are every contribution already in the accumulator.
  - The **accumulator** is the composition of the earlier steps, rebuilt and retained, with
    atomic grouping lifted because it is an input, never offered for acceptance.
- **A candidate whose independent checks failed** (§7.3 step 6). Members and contents are
  every contribution in it, because nothing narrows an interaction without a qualified
  analysis.

  A failed check is an **opaque identifier and nothing else**: a check's own messages can
  carry its expectations, and a resolver must not be handed the answer.

**Synthesized group.** It records:
- the request and the decision;
- the members as constituents;
- the **contents**: every contribution the new candidate holds;
- `derived_from` naming all of the contents;
- the resolver, the brief and the attempt;
- the baseline, the accumulator and the candidate;
- `requires_verification`.

### A request is rebuilt from its record (F1)

`resolve` takes only the request's `RecordRef`. `load_request` then:
- reads the archived record;
- re-reads every contribution it names, which must still be retained and match;
- re-retains the baseline commit and re-checks the accumulator commit against its snapshot;
- **recomputes** the decision key, the scope and the protected paths with the same code
  that raised the request.

A record that disagrees is refused as `record_mismatch`. `ResolutionRequest`'s fields are
private and read through accessors, so nothing a caller holds in memory can widen a
request. In the first version they were public, and a caller could:
- forge the group key, resetting the allowance;
- add `.github/` or `.aethyme/` to the scope and clear the protected list;
- add a preference after the request was raised.

### Protected paths (F2)

Matching is by path component, after folding each component as a case- and
normalization-insensitive file system would:
- NFC;
- HFS+ ignorable code points removed;
- an NTFS stream suffix and trailing dots and spaces removed;
- lowercased.

On APFS, `.AETHYME/gates.toml` is the same file as `.aethyme/gates.toml`.

A path is protected when any of these holds:
- **A protected directory:** a component, at any depth, is `.aethyme`, `.github`,
  `.cargo`, `.config` or `.git`. `.config/nextest.toml`'s `default-filter = "none()"` would
  turn the cargo-test gate into a no-op.
- **A protected file:** the file name, at any depth, is `.gitattributes`, `.gitmodules`,
  `rust-toolchain`, `rust-toolchain.toml`, `Cargo.toml`, `Cargo.lock` or `build.rs`.
- **Harness:** it is, or is under, a requirement's harness path. Requirement paths are
  normalized first: `./x`, `/x`, `x//y` and `x/` are all `x`, and `..` is refused.
- **The baseline's own policy:** a gate command in the baseline's `.aethyme/gates.toml`
  names it, or the baseline's security review rule lists it (`.aethyme/config.toml`, rules
  requiring `security`, matched case-insensitively). Both are read from the archived
  baseline, never from a worktree or the proposal.

**Symlinks:** a write to a path that is a symlink is refused, whatever its new kind, and
so is a write that creates one. A write that makes a file executable is refused too.

### The allowance is per decision (F3)

The key is the decision:
- its kind;
- every selected contribution that touches it, by base and result snapshot, applied yet or
  not;
- for a conflict, the conflicting paths.

So all three orders of a three-way conflict on one line are one decision with two
attempts; in the first version, they were three pairs with six.

Attempts are also shared across decisions of the same kind that contain one another. A
failed-checks request padded with an unrelated contribution, or trimmed of one, draws on
the same allowance.

Each attempt is charged in its own transaction **before** dispatch. An infrastructure
failure or a panicking resolver still costs it.

### A synthesized candidate holds and names everything (F4)

- **Its inputs** are the contents plus the members resolved into it, and the group record
  names them all.
- **A dropped change is unresolved.** On a path a contribution changed, other than a
  conflicting path itself, the candidate may not hold that contribution's base version
  where the change was present (or, for the conflicting step, owed). This applies to
  conflicts and failed checks alike.
- **Last writer:** a conflicting path left as either side had it, without an authorized
  preference for that side, is also unresolved.

### Comparisons ignore whitespace (F5)

The last-writer and dropped-change guards compare content after normalization:
- trailing whitespace on every line ignored;
- CRLF read as LF;
- trailing blank lines ignored.

Binary content is compared exactly. A last writer with a trailing space, or without its
final newline, is still a last writer.

### Every attempt is settled (F6)

An error after dispatch becomes a recorded outcome and settles the ledger row:
- `scope_violation` when the archive refuses to name the proposal (a filter attribute, an
  unsupported entry, an invalid snapshot);
- `infrastructure_deferred` otherwise.

The failure states are exactly `unresolved`, `inconclusive`, `scope_violation`,
`infrastructure_deferred` and `budget_exhausted`. None of them carries a candidate.

### Every candidate is unverified

Nothing marks a `Candidate` verified, and no field can be set to make it so. A resolver's
candidate is produced by `Producer::Resolver` with mode `synthesized`. Acceptance (L5)
must require a passing verification of the exact subject before it promotes or publishes
anything. This module only ever hands it a proposal.

### Alternate views (§7.4)

A synthesized X records its contents in `derived_from`. "X without B" uses the composer's
`recompose_without`: it composes the remaining **originals** into a new candidate, whose
subject is never X's. Without the constituents' retained records, the request is refused
with `inseparable_selection`.

### The allowance ledger is created on first use

`resolution_attempts` in `state.db` is created with `CREATE TABLE IF NOT EXISTS` when
first charged, not as a numbered migration. The L3 stack (#718, #730) already claims schema
5 and 6, and a number taken here would make a store migrated by one stack skip the other
stack's tables.

No other reader consults this table, and an older binary ignores it. When the stacks
merge, it becomes the next numbered migration, and that migration **must adopt a table
this statement already created**: keep `IF NOT EXISTS`, and add any new columns with
`ALTER TABLE` guarded by a column check.

## Measured (provisional fixtures, blind API)

The pipeline runs each scenario as: compose; write the manifest; for a candidate,
reconstruct it from the archive and check it; raise a request when the check fails.

| | Composer alone (#664) | With checks and requests |
|---|---|---|
| Provisional scenarios accepted | 15/17 | 16/17 |
| Held-out scenarios accepted | 8/12 | 9/12 |

**These gains measure the plumbing, not the system.** The stand-in for the gates is the
fixtures' own oracle (`judge`), the same judge that scores the scenario. The one added
scenario per set (FX04, HX04) is the oracle rejecting a clean but wrong candidate, which
the pipeline turns into `resolution_required`. That shows a failing check becomes a
request. It says nothing about what real gates would catch. Only an opaque check
identifier leaves the stand-in, and a test asserts that no oracle message appears in a
request record.

**The rename-propagating test double is format-specific.** It rewrites `id="…"` renames
into `"…"` and `"#…"` references. It turns FX04's request into a candidate the oracle
accepts. HX04 is a structural twin of FX04 (another identifier rename across HTML and
TypeScript), so its pass is weak evidence that the double generalizes, and none that a
real resolver would.

FX02 s1 (move plus edit) and HX01, HX02 and HX03 s1 still conflict. A structural engine
(E1, D04) is required.

## Open

- **Rooting:** manifests, requests and synthesized candidates have no retention root yet,
  so reclamation may remove them; keeping them is L5's job.
- **Narrowing:** for failed checks the decision is the whole candidate. A qualified
  analysis could narrow it.
- **Real checks:** in production the check is the gate run on the complete candidate. That
  wiring waits for #725.
- **Objectives:** the allowance is per decision, not per objective, because no objective
  record exists yet.
- **Forged archive records:** the recomputation guards against them, but only a crate
  test that writes one could exercise it. The public API cannot write such a record.
