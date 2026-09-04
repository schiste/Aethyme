# Version-safe graph refresh

Last Updated: 2026-09-03

Aethyme repositories may make committed graph fragments authoritative through
an explicit canonical deployment opt-in:

```bash
aethyme deploy --repo . --with-graph
# When no canonical origin is configured:
aethyme deploy --repo . --with-graph --graph-repository owner/name
```

Default and local-only deployment remain graph-free. The opt-in writes the
`[graph]` policy in `.aethyme/config.toml` and `.aethyme/engine-version`, but
does not generate anything. Review and commit enrollment first so generation
has an exact committed source. Refresh those fragments only
with the installed `aethyme` release whose version exactly matches
`.aethyme/engine-version`. The router links the compatible indexer and engine
libraries into the same signed release unit; no separately installed indexer
or network download is involved.

## Review and apply

Start with the read-only status and plan:

```bash
aethyme graph status --repo . --json
aethyme graph refresh plan --repo . --diff
aethyme graph refresh plan --repo . --json
```

Status reads typed policy and committed graph provenance directly; a disabled
repository is a healthy no-op and does not clone or index. The refresh proposal
is built from exact committed `HEAD` in a disposable checkout.
Its digest binds the source commit and tree, graph policy and pin, installed
component versions, every proposed path/hash/mode, derived-store action, dirty
overlap, live sessions, and relevant leases. The plan contains hashes and
repository-relative paths, never source contents or absolute paths.

After reviewing the plan, authorize only its full digest:

```bash
aethyme graph refresh execute --repo . --confirm <plan-sha256>
```

Execution revalidates the complete plan under an exclusive lock. It writes
committed fragments transactionally, verifies their hashes, then atomically
publishes the local `.aethyme/graph_store.redb`. Review and commit changed
fragment paths. Because that commit creates a new authoritative HEAD, rerun
`graph status`; if it reports only a stale derived store, review and execute
the new plan to stamp the local store against that committed graph revision.

Every other clone or broker worktree can then build only its ignored local
query artifact from the verified committed fragments:

```bash
aethyme graph materialize --repo .
aethyme graph materialize --repo . --json
```

Refresh writes a deterministic `.aethyme/graph/manifest.json` that binds the
non-graph Git tree, repository identity, engine version, and complete fragment
set. Materialization validates that manifest and decodes only exact committed
fragment bytes: it never clones the repository, parses source, or regenerates
fragments. It refuses disabled authority, stale or corrupt fragments, a
version mismatch, or a HEAD that moves during validation. It changes no
tracked file and reports the full source SHA, action, file count, elapsed
milliseconds, and zero clone/index work. An already-current store is a no-op.

## Safety and recovery

- A mismatched engine pin requires the signed compatible Aethyme release or a
  reviewed repository upgrade. Refresh never rewrites the pin.
- Dirty paths that overlap the exact graph write set block. Disjoint dirty
  work is preserved and is explicitly excluded from plan inputs.
- Live broker sessions block shared fragment changes. Finish them before
  applying the plan; relevant leases remain visible in JSON.
- Symlinked output paths are refused.
- A crash leaves a private digest-bound recovery journal. Do not rerun execute.
  Complete only the reviewed transaction with:

  ```bash
  aethyme graph refresh recover --repo . --plan <plan-sha256>
  ```

Local-only Aethyme activation cannot enable authoritative fragments because
its policy is intentionally untracked. Use canonical deployment when a team
wants a shared graph contract.

Explore remains read-only in every mode. Without a usable local store it
returns successful, schema-valid degraded answer JSON: `answer` is empty,
answer/navigation safety are false, observability names the graph state, and
the next action either offers explicit materialization or notes that graph
support is optional. Agents may continue with bounded source verification;
they must not enable graphing simply to silence degradation.

The lower-level `aethyme-engine-cli index` command remains supported for
engine diagnostics and compatibility. It is not a substitute for the reviewed
refresh lifecycle when committed fragments are repository authority: it does
not propose their exact write set or bind repository preconditions to a digest.

## Upgrading an old pin

Install the release named by `.aethyme/engine-version` when it is still
supported. When a repository migration intentionally advances that pin, use
the repository upgrade planner and its digest-confirmed apply flow first.
Then use the graph refresh plan above. Never edit the pin merely to make a
locally installed binary pass: doing so would change repository policy without
reviewing the corresponding generated graph.

For phase-level timing, byte, count, memory, and disk-footprint measurement,
use the reproducible [graph performance evidence](graph-performance.md)
workflow against disposable Playground repositories.
