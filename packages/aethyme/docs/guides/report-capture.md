# Capture And File Broker Reports Safely

Last Updated: 2026-08-23

Broker reports turn local operational evidence into a reviewable GitHub issue
without sending anything during capture or rendering. The safe workflow is:

1. **Capture** an allowlist-only JSON snapshot offline.
2. **Review** the snapshot and the rendered issue Markdown.
3. **Confirm** the SHA-256 of the final reviewed Markdown bytes.
4. **File** those exact bytes through the coordinated GitHub operation layer.

For the complete flag inventory, see the
[CLI reference](../reference/cli.md).

## Private-Repository Warning

> **Do not assume a redacted report from a private repository is safe for a
> public issue tracker.** Redaction removes forbidden content, but retained
> operational metadata can still disclose repository identity, branch names,
> gate names, timestamps, platform details, commit or tree hashes, and relevant
> repository-relative paths. A relative path can reveal private architecture
> even though it does not reveal the local checkout location.

Before filing, verify both the final Markdown and the exact `--repo owner/name`
destination. This matters especially when the source repository is private and
the destination repository is public or visible to a broader organization.

Task text and coordinated-operation authorization reasons are excluded by
default. `--include-task` explicitly adds them; use it only when their diagnostic
value outweighs the disclosure risk. The flag does not make that text safe—it
only records the operator's decision to include it.

## 1. Capture Offline

Run capture from the repository or a registered broker worktree:

```bash
aethyme broker report capture --kind bug \
  --title "Submit gate failed" \
  --output submit-gate.json
```

The command atomically creates
`.aethyme/reports/submit-gate.json`, prints its SHA-256, and performs no network,
Git remote, graph, or gate operation. Initialized repositories ignore
`.aethyme/reports/`. Existing artifacts are never overwritten.

The default report is built field by field from an explicit allowlist. This
shortened example shows the shape of a captured report; omitted sensitive fields
are absent rather than replaced with masking strings:

```json
{
  "schema_version": 1,
  "kind": "bug",
  "title": "Submit gate failed",
  "captured_at": 1787479200000,
  "snapshot": {
    "schema_version": 1,
    "includes_task": false,
    "build": {
      "version": "0.1.0",
      "commit": "0123456789abcdef0123456789abcdef01234567"
    },
    "platform": {
      "os": "macos",
      "arch": "aarch64"
    },
    "session": {
      "id": 150,
      "branch": "agent/reproduce-submit-failure",
      "origin": "spawned",
      "status": "active",
      "diff_base": "89abcdef0123456789abcdef0123456789abcdef"
    },
    "recent_event_types": [
      {
        "id": 901,
        "recorded_at": 1787479199000,
        "kind": "gate.finished",
        "session_id": 150
      }
    ],
    "operations": [],
    "gates": [
      {
        "gate": "cargo-test",
        "tree_hash": "fedcba9876543210fedcba9876543210fedcba98",
        "status": "fail",
        "failure_class": "test_failure",
        "cache_source": "executed",
        "exit_code": 101,
        "duration_ms": 1842,
        "recorded_at": 1787479199000,
        "triggered_by": "rust/crates/aethyme-broker/src/report.rs"
      }
    ],
    "last_known_failure": {
      "source": "gate",
      "gate": "cargo-test",
      "tree_hash": "fedcba9876543210fedcba9876543210fedcba98",
      "recorded_at": 1787479199000,
      "status": "fail",
      "failure_class": "test_failure",
      "exit_code": 101,
      "cache_source": "executed"
    }
  }
}
```

By default the snapshot cannot contain:

- file contents, diffs, or hunks;
- command arguments, logs, standard output, or standard error;
- absolute filesystem paths or arbitrary event payloads;
- task text or operation authorization reasons.

The allowlist is a data-minimization boundary, not a guarantee that all retained
metadata is non-sensitive. If more context is needed, add it during human review
instead of broadly opting in to task text.

## 2. Review The Source And Rendered Issue

Inspect the captured document and its current digest locally:

```bash
aethyme broker report show submit-gate.json
```

Then render it against a repository issue form:

```bash
aethyme broker report render submit-gate.json \
  --form bug_report.yml \
  --output submit-gate.issue.md
```

Rendering reads `.github/ISSUE_TEMPLATE/bug_report.yml`, preserves the form's
field order, and makes unknown or unavailable fields explicit as `Unfilled`.
It performs no network operation. Edit
`.aethyme/reports/submit-gate.issue.md` until every required section is present
and no generated `Unfilled` marker remains.

Review the entire final artifact, including metadata such as branch names,
relative paths, hashes, gate names, and platform details. Also verify that the
issue title reconstructed from the artifact is appropriate for the destination.

## 3. Confirm The Final Bytes

Compute the digest **after the last edit**:

```bash
# macOS
shasum -a 256 .aethyme/reports/submit-gate.issue.md

# Linux
sha256sum .aethyme/reports/submit-gate.issue.md
```

Use this digest, not the digest printed for the original JSON capture. The
confirmation binds authorization to the exact reviewed `.issue.md` bytes. Any
change after review causes filing to fail before GitHub is invoked.

## 4. File Through The Broker

From the registered broker worktree, file the reviewed artifact into the exact
repository you inspected:

```bash
aethyme broker report file .aethyme/reports/submit-gate.issue.md \
  --repo owner/name \
  --confirm <full-lowercase-sha256>
```

Filing revalidates the digest and required sections, then runs `gh issue create`
through the broker's coordinated operation layer. On success it journals the
issue URL and number and marks the source capture as filed by digest. Confirm
the result locally with:

```bash
aethyme broker report show submit-gate.json
```

## Ambiguous GitHub Outcomes

If GitHub returns a non-zero result, an unparseable success response, or the
issue identity cannot be persisted, the broker records `outcome_unknown` and
prints an operation ID. **Do not rerun `report file`.** First inspect the target
repository for an issue that may already have been created, then reconcile the
recorded operation:

```bash
aethyme broker operations reconcile --operation <operation-id> \
  --outcome succeeded \
  --reason "Verified the issue exists in owner/name"
# or, only after proving no issue was created:
aethyme broker operations reconcile --operation <operation-id> \
  --outcome failed \
  --reason "Verified no issue was created in owner/name"
```

A reconciled success continues to block duplicate filing. A reconciled failure
allows a later filing attempt, which still requires a fresh digest confirmation
of the artifact being submitted.

## Review Checklist

Before `report file`, confirm all of the following:

- the destination `owner/name` has the intended visibility and audience;
- the final Markdown contains no private identifiers or architecture details
  that should remain private;
- `--include-task` was omitted unless task text and operation reasons were
  deliberately reviewed for disclosure;
- every required issue-form section is complete;
- the supplied SHA-256 was computed after the final edit;
- there is no unresolved `outcome_unknown` filing operation for the report.
