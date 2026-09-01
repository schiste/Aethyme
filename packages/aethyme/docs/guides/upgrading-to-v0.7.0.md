# Upgrading to Aethyme v0.7.0

Last Updated: 2026-09-01

v0.7.0 adds an opt-in, GitHub-native evidence adapter for reviewers that
comment but do not submit a formal approval. Existing repositories keep the
formal `github_approval` behavior unless they explicitly change policy.

## Compatibility

| Contract | v0.7.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 24; writes schema 24 |
| Repository deployment | schema 1; no mandatory migration from v0.6.0 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `review.evidence_adapter = "github_check_run"` can replace formal approval
  evidence for an opted-in repository.
- The adapter requires an exact check name, trusted GitHub App slug, current
  full PR head SHA, `completed` status, and `success` conclusion.
- Missing, wrong-app, stale, unsuccessful, unavailable, or truncated evidence
  refuses the transition without mutation.
- `review unlock` polls live evidence, so a successful check can satisfy
  review without a webhook.
- Review-gated ship execution revalidates the same configured evidence before
  publishing.

The broker never parses or stores reviewer comment bodies. A repository-owned
check translates provider-specific evidence—such as a trusted actor's
structured summary and unresolved threads—into the GitHub check result.

## Before upgrading

Finish active sessions when practical and confirm the installed router and
engine sibling come from the same installation manager. No broker-storage or
repository migration is required.

Repositories using formal approvals need no policy change. To use a
comment-only reviewer, first protect the repository-owned gate that interprets
that reviewer's evidence. Prefer a dedicated GitHub App. If the check is
created by `github-actions`, ensure a pull request cannot replace or spoof the
workflow that owns the configured check name.

## Install or update

Homebrew users update both binaries as one formula-managed unit:

```bash
brew update
brew upgrade aethyme
```

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

No configuration change is mandatory. An opt-in check-backed policy looks
like this:

```toml
[review]
schema_version = 1
enabled = true
provider = "github"
evidence_adapter = "github_check_run"
required_approvals = 0
evidence_check_name = "review-gate/codex"
evidence_app_slug = "github-actions"
unlock_adapter = "github_label"
unlock_label = "aethyme-validation-ready"
```

Verify the installed pair and ordinary broker loop after updating:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
```

For a configured review lifecycle, inspect `broker review show --json`, then
run `broker review unlock`. A missing or unsuccessful check should refuse
without applying the configured label or workflow mutation.

## Rollback

Restore both v0.6.0 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol schemas
are unchanged, so no database or repository deployment rollback is required.

Before running v0.6.0 in a repository that opted into `github_check_run`,
restore the v0.6.0-compatible formal-approval policy: remove
`evidence_adapter`, `evidence_check_name`, and `evidence_app_slug`, then set
`required_approvals = 1`. v0.6.0 intentionally rejects unknown review-policy
fields rather than ignoring them.

Never combine a v0.6.0 router with a v0.7.0 engine sibling, or the reverse.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Homebrew installations must be upgraded through Homebrew.
- The check-run adapter bounds one query to 100 latest exact-name results and
  refuses a truncated response.
- Aethyme does not interpret arbitrary comments or reactions. Repositories
  must own and protect the check that converts reviewer-native evidence into
  a successful exact-head result.
