# Changelog

All notable user-visible changes to Aethyme are documented here. Release
artifacts and their exact source revision are recorded in each signed
`release-manifest.json`.

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
