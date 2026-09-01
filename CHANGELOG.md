# Changelog

All notable user-visible changes to Aethyme are documented here. Release
artifacts and their exact source revision are recorded in each signed
`release-manifest.json`.

## [0.7.1] - 2026-09-01

### Added

- Closed review lifecycles can be reassigned to an exact-head live session or
  explicitly abandoned without deleting state, evidence, or generation
  history.
- Checkpoint recovery plans expose stable refusal codes and ordered,
  preservation-first next actions in JSON.

### Changed

- Closed sessions remain available for diagnostics but cannot claim leases or
  run review mutations.
- `broker repair` is limited to recorded submit and promoted-path conflicts;
  checkpoint drift now routes directly to the dedicated checkpoint planner.
- The maintainer release contract now records patch increments as the default;
  changing either leading version component requires explicit maintainer
  authorization.

### Fixed

- A session closed during draft/review coordination no longer leaves its pull
  request identity permanently locked without a supported recovery path.
- Rejected closed-session lease claims no longer persist phantom ownership.
- Unsafe checkpoint recovery no longer recommends repeatedly rebasing onto an
  integration history that can contain unrelated promoted work.

### Upgrade notes

Read [Upgrading to v0.7.1](packages/aethyme/docs/guides/upgrading-to-v0.7.1.md)
before updating. Broker storage migrates from schema 24 to 25; repository
deployment and engine protocol remain unchanged.

## [0.7.0] - 2026-09-01

### Added

- Review policies can opt into `github_check_run` evidence with an exact check
  name and trusted GitHub App slug, while formal GitHub approval remains the
  default.
- Sanitized evidence records the selected check ID, status, conclusion, app,
  and exact head SHA without retaining review comments or arbitrary provider
  payloads.

### Changed

- `broker review unlock` polls configured review evidence and can advance a
  check-backed lifecycle directly from `review_requested`; comment-only
  reviewers no longer require a synthetic approval webhook.
- Review-gated publication revalidates the configured evidence adapter against
  the exact pull-request head immediately before publishing.

### Fixed

- Automated reviewers that report through comments and a repository-owned
  check are no longer permanently excluded by the hardcoded
  `reviewDecision == APPROVED` condition.
- Wrong-app, stale-head, unsuccessful, unavailable, and truncated check-run
  evidence fails closed without running the validation-unlock mutation.

### Upgrade notes

Read [Upgrading to v0.7.0](packages/aethyme/docs/guides/upgrading-to-v0.7.0.md)
before updating. Broker storage, repository deployment, and engine protocol
schemas are unchanged from v0.6.0; no mandatory migration is required.

## [0.6.0] - 2026-09-01

### Added

- `aethyme broker worktree-root` provides a read-only, structured placement
  plan with the canonical checkout identity, clone-specific key, preferred
  external root, and constrained fallback.
- Session-start reports now retain the selected worktree root, its source, the
  scanner boundary, and any fallback reason in text and JSON.

### Changed

- Broker-managed worktrees now live in private per-user host state outside the
  repository by default. Independent same-named clones receive distinct roots,
  and starts invoked from a broker worktree create siblings rather than nested
  checkouts.
- Existing `.aethyme/worktrees/` sessions remain cleanup-compatible and serve
  only as a reported fallback when the platform host-state root is unavailable.

### Fixed

- Repository-wide scanners no longer traverse broker-managed nested checkouts,
  preventing duplicate findings and runaway recursive work.
- Explicit roots inside the repository or another linked worktree are refused,
  while private ownership markers keep external cleanup fail-closed.

### Upgrade notes

Read [Upgrading to v0.6.0](packages/aethyme/docs/guides/upgrading-to-v0.6.0.md)
before updating. Broker storage, repository deployment, and engine protocol
schemas are unchanged from v0.5.0; no migration is required.

## [0.5.0] - 2026-09-01

### Added

- Gate scope manifests, authenticated external event ingestion, review
  lifecycles, lease-routing exports, and reviewed publication policies provide
  auditable coordination without making repository-specific integrations part
  of the broker core.
- Broker retention and garbage-collection policies bound historical state and
  safely reclaim represented session worktrees, branches, events, operations,
  advisories, and expired resource leases.
- Atomic first enrollment publishes the complete repository contract or leaves
  the repository unchanged.

### Changed

- Publication evidence now controls exposure resolution: promoted paths remain
  visible until the exact entry is verified on remote main or is explicitly
  reconciled as an equivalent landing.
- Status, finish, cleanup, gate diagnostics, checkpoint recovery, and planned
  lease conflicts now report bounded, preservation-first next actions.
- Shared remote publication can require configured reviews and policy evidence;
  the default remains backward-compatible until those controls are enabled.

### Fixed

- Submission provenance, equivalent-tree leases, divergent upstream counts,
  amended promoted checkpoints, and `git -C` operations are classified from
  their actual Git evidence instead of inferred wording or commit identity.
- Protected-branch commits require an active broker session, while ship can
  synchronize a clean main checkout without rejecting unrelated untracked
  files.
- Enhance verification reports the exact tracked provenance used to generate
  onboarding, avoiding false freshness claims.

### Upgrade notes

Read [Upgrading to v0.5.0](packages/aethyme/docs/guides/upgrading-to-v0.5.0.md)
before updating. Repository deployment stays at schema 1 and engine protocol
stays at 1. Broker storage advances to schema 24 and migrates automatically;
review the rollback limitation before first opening an existing broker database.

## [0.4.2] - 2026-08-29

### Added

- Operators can inspect retained broker-owned worktrees with
  `aethyme broker cleanup --all-cleaned` and explicitly apply the unchanged
  sweep with `--apply`.
- Cleanup plans expose per-worktree eligibility and estimated reclaimable
  bytes in both text and JSON output.

### Fixed

- Cleanup safety now recognizes commits represented by local integration or
  configured upstream, so a successfully promoted session is not retained
  merely because the primary local main checkout has not been synchronized.
- Bulk cleanup revalidates every candidate and leaves adopted, dirty,
  symlinked, unsafe, uninspectable, or unrepresented worktrees untouched.
- Broker status warns when safely reclaimable cleaned worktrees remain.

### Upgrade notes

Read [Upgrading to v0.4.2](packages/aethyme/docs/guides/upgrading-to-v0.4.2.md).
Broker storage, engine protocol, and repository deployment schemas are
unchanged; no repository migration is required.

## [0.4.1] - 2026-08-29

### Fixed

- Regenerated the checked-in AGENTS and CLAUDE protocol so the safe,
  preservation-first remediation for unsupported session merge commits is
  present in the released repository.
- Refreshed deterministic onboarding freshness metadata after the v0.4.0
  release inputs changed, restoring a clean `aethyme enhance verify` result
  on the exact release tree.

### Upgrade notes

Read [Upgrading to v0.4.1](packages/aethyme/docs/guides/upgrading-to-v0.4.1.md).
Binary protocols and repository schemas are unchanged from v0.4.0; no
repository migration is required.

## [0.4.0] - 2026-08-29

### Added

- Rewritten session checkpoints have a digest-confirmed, preservation-first
  recovery plan instead of requiring an unsafe baseline reset.
- Blocked external-main reconciliation exposes a complete schema-2 resolution
  template with exact identifiers, structured evidence, field rules, and an
  atomic no-clobber writer.
- Broker command failures persist in allowlist-only diagnostic reports without
  command output, task text, diffs, file contents, or secrets.

### Changed

- Submission planning selects an explicit safe base, explains unsupported
  owned merge commits, and removes failed planning entries instead of leaving
  misleading submitted queue residue.
- Submission verification distinguishes missing gate configuration, no
  triggered gates, fresh execution, cache reuse, and failure.
- Guarded execution attributes changes to already-dirty files as well as newly
  dirty paths, so both remain subject to explicit lease ownership.

### Fixed

- Closed sessions can no longer authorize new coordinated Git or GitHub
  operations.
- External-main reconciliation recognizes stable patch-equivalent landings
  when local main already equals upstream and avoids replaying an empty commit.
- Session start refuses implicit or ambiguous bases rather than inheriting an
  unsafe checkout position.

### Upgrade notes

Read [Upgrading to v0.4.0](packages/aethyme/docs/guides/upgrading-to-v0.4.0.md)
before updating. Broker storage remains schema 17, engine protocol remains 1,
and repository deployment remains schema 1, so v0.3.0 repositories do not
require a mandatory policy migration.

## [0.3.0] - 2026-08-27

### Added

- Durable, non-blocking promotion advisories and queue-entry path exposures
  remain visible until verified publication or confirmed reconciliation.
- Operation history supports stable filtering, pagination, exact inspection,
  and evidence-based reconciliation of failed or ambiguous pushes.
- Repository upgrades expose exact content-and-mode plans, local diffs,
  customization resolutions, transactional apply, and crash recovery.
- Session handoffs, lease planning, gate provenance, cache bypass, canonical
  remote coordination, and host-wide resource leasing are available through
  stable broker surfaces.

### Changed

- Submission planning replays only session-owned commits from the accepted
  checkpoint and classifies ancestry, patch identity, and ambiguity before
  integration.
- Active sessions pin their repository contract, keeping diagnostic,
  recovery, finish, reporting, and eligible pre-commit lanes available across
  binary updates.
- Generated onboarding derives repository identity and surfaces from the
  tracked snapshot and uses content-addressed, reproducible freshness data.
- Generated AGENTS and CLAUDE policy teaches advisory delivery, persistence,
  acknowledgment, rebase, session-close, and publication lifecycles.

### Fixed

- Canonical remote identity and host-wide write barriers now serialize Git and
  GitHub mutations across independent clones without retaining credentials.
- External-main reconciliation preserves reviewed unrecorded work and updates
  integration and queue state under a digest-confirmed transaction.
- Failed exact pushes are classified from destination-ref evidence rather
  than stderr, while mixed or missing evidence remains safely unknown.
- Synthetic submission commits preserve reviewed contract decisions without
  copying arbitrary session commit bodies into integration.

### Upgrade notes

Read [Upgrading to v0.3.0](packages/aethyme/docs/guides/upgrading-to-v0.3.0.md)
before updating an enrolled repository. This release advances broker storage
to schema 17. Repository deployment remains schema 1, but regenerated policy
and onboarding should be reviewed per repository.

## [0.2.2] - 2026-08-24

### Added

- Host-wide resource coordination allocates ports, Docker namespaces,
  database names, and capacity slots as atomic lease bundles with TTL,
  heartbeat, ownership credentials, and generation-fenced reconciliation.
- Gate definitions can declare shared host resources. Broker gate execution
  acquires those resources while preserving independent local test processes.
- The opt-in pre-push adapter proves the outgoing commit and runs the selected
  broker gates without making pre-commit hooks depend on shared services.
- `aethyme upgrade plan` and digest-confirmed `aethyme upgrade apply` migrate
  Aethyme-owned repository policy from logic embedded in the installed binary.

### Changed

- Canonical deployments track `.aethyme/repository.json`; local-only
  deployments keep their repository schema marker clone-local and ignored.
- Enrolled repositories fail closed on broker use when their generated policy
  is missing a required migration or is newer than the installed binary.
- Release manifests advertise repository schema compatibility, and successful
  paired-binary updates name the explicit per-repository follow-up.

### Security and reliability

- Repository upgrade plans bind Git HEAD, deployment mode, relevant file
  state, planned paths, and migrations into a full SHA-256 confirmation.
- Upgrades refuse dirty worktrees, mismatched confirmations, malformed or
  future markers, and managed paths that escape through symlinks.
- An in-progress marker prevents an interrupted migration from appearing
  current.

### Upgrade notes

Read [Upgrading to v0.2.2](packages/aethyme/docs/guides/upgrading-to-v0.2.2.md)
before updating an enrolled repository. This release advances broker storage
to schema 8 and introduces repository deployment schema 1.

## [0.2.1] - 2026-08-24

### Changed

- Stable release formula generation now emits Homebrew-audit-clean output
  while continuing to install both product binaries from one archive.

### Fixed

- Reusing an active broker session preserves its recorded submission baseline,
  so rebased or patch-equivalent commits cannot silently disappear from the
  ownership calculation.
- Submission, reuse-drift reporting, and finish handoffs now derive pending
  work from the same normalized submission plan.
- Patch-equivalent submissions whose content is already integrated are
  recorded as superseded without rerunning gates or creating an empty
  promotion commit.

### Upgrade notes

Read [Upgrading to v0.2.1](packages/aethyme/docs/guides/upgrading-to-v0.2.1.md)
for compatibility, verification, rollback, and known-issue guidance.

## [0.2.0] - 2026-08-24

### Added

- A local-first broker workflow covering isolated sessions, leases, guarded
  commands, gates, normalized submission, integration promotion and shipping,
  durable finish handoffs, and redacted offline issue reports.
- Read-only ship, lease, reconciliation, semantic-gate, submission-provenance,
  and report-planning surfaces with stable JSON output.
- Paired prebuilt binaries for Apple Silicon macOS, Intel macOS, and x86-64
  Linux, plus a stable-channel installer and explicit version pinning.
- A Sigstore-signed release manifest containing the exact source SHA,
  supported platforms, both required binaries, artifact sizes and SHA-256
  digests, compatibility boundaries, minimum Git version, and release channel.
- A public `schiste/homebrew-tap` formula that installs the router and engine
  from one archive and participates in normal `brew update` / `brew upgrade`.
- Explicit `aethyme update check`, `update plan`, and digest-confirmed
  `update execute` commands for installer-managed binary pairs.

### Changed

- The product and development test stack are now entirely Rust. The retired
  `python -m src.cli` entry point is a deliberate hard break with no shim.
- All production crates inherit one workspace version. Both `aethyme` and
  `aethyme-engine-cli` report that version and the embedded source description.
- Commit hygiene is driven by one typed policy. Bodies remain mandatory for
  `fix`, `feat`, `refactor`, and `perf`; non-substantive types may be
  subject-only; inline section content is accepted.
- Submission simulation replays session-owned patches onto integration and
  classifies exact and stable patch-equivalent history instead of treating two
  commit identities as an undifferentiated merge.
- The portable installer and native updater use one versioned pair layout with
  an atomic activation link and one retained rollback bundle.

### Fixed

- Doctor version repair now installs and verifies both required binaries.
- Task-next output deduplicates top anchors from the same file while preserving
  first-seen ranking across map and redb paths.
- External default-branch movement can be planned and reconciled without
  blanket deletion of unrecorded work.
- Successful pre-commit gates stay quiet, while failures retain their complete
  output and exit diagnosis.

### Security and release integrity

- Release archives are smoked after extraction on every supported target.
- Standalone checksums and `SHA256SUMS` are published; the manifest and
  installer digest are covered by a keyless Sigstore bundle.
- Semantic graph gate suggestions remain advisory and never expand enforced
  gates or submit-time checks.
- Report snapshots use an explicit allowlist and omit task text, reasons,
  absolute paths, file contents, diffs, and hunks by default.

### Upgrade notes

Read [Upgrading to v0.2.0](packages/aethyme/docs/guides/upgrading-to-v0.2.0.md)
before upgrading an existing broker repository. It covers the paired-binary
requirement, broker database backup/migration, graph-store regeneration,
compatibility, rollback, and known issues.
