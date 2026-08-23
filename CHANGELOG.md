# Changelog

All notable user-visible changes to Aethyme are documented here. Release
artifacts and their exact source revision are recorded in each signed
`release-manifest.json`.

## [0.2.0] - Unreleased

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
