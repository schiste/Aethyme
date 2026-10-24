# Git options for broker worktrees

Last Updated: 2026-10-24

This assessment covers #901, #902, #904, #906 and #907. It does not
enable repository settings. The bounded experiment is
`packages/aethyme-eval/benchmarks/performance/git_worktree_options.py`.
Run it explicitly with a new output directory and an existing fixture parent;
it creates and removes only its own synthetic Playground. Failed runs retain
their exact fixture path and command receipts for inspection.

## Observations

One macOS 27.2 internal-drive run used Git 2.55.0, with Apple Git 2.54.0
as the alternate reader/writer. Each variant had 1,800 tracked text files in
12 directories. Seven consecutive status samples were taken per state.
The untracked case added 200 files and 200 ignored 1 KiB build artifacts.
All three variants returned byte-identical porcelain status for each state;
both clients agreed before and after an alternate-client index refresh.
The local untracked-cache timestamp probe passed for each variant.

| Option | Clean median / maximum ms | Dirty median / maximum ms | Untracked median / maximum ms | Index bytes |
|---|---:|---:|---:|---:|
| Baseline | 19.62 / 21.25 | 20.76 / 22.46 | 16.00 / 21.15 | 158,940 |
| manyFiles | 16.88 / 29.24 | 16.91 / 18.04 | 19.77 / 20.80 | 130,901 |
| fsmonitor | 21.15 / 25.08 | 12.82 / 23.55 | 28.08 / 39.06 | 159,018 |

The first fsmonitor observation took 67.06 ms, versus 20.84 ms baseline.
One index add took 14.67 / 16.28 / 29.33 ms respectively. These are
single index-write observations, not distributions. Child CPU samples are
recorded individually; they exclude the fsmonitor daemon's CPU and memory.

Raw reports include every command, return code, stderr, timing and harness
digest. The recorded harness digest is
`1e175674fa3b3ac5ee6f2c3414065d8d2dcb4c41a30a0b19a73abbda8290d18f`.
These are first-observation and warm samples, not cold-cache samples. Fixed
variant order, one filesystem, small fixtures and two recent clients limit
the conclusions. There is no broker, dependency-preparation or gate speedup
claim. The monitor was stopped and its stopped state checked before removal.

## Decisions and remaining qualification

### manyFiles (#902): keep opt-in

The small clean/dirty gains did not extend to the untracked case. Index
size decreased about 18%, but these numbers do not justify changing every
repository's defaults. This run verifies two recent clients, not a minimum
supported version. Keep the whole bundle off by default. Before wider
adoption, measure linked worktrees, external roots, larger repositories,
checkout/index-write distributions and older supported clients. Preserve
the untracked-cache filesystem probe and verify skipped-checksum
interoperability rather than inferring it from version numbers.

### fsmonitor (#901): keep opt-in

Dirty status improved in this run; clean and untracked medians worsened.
Startup and daemon ownership also have costs. Leave the default unchanged.
External-drive runs, actual broker calls, daemon RSS/CPU, older clients and
network filesystem behavior remain unqualified. Stop the exact repository's
monitor before deleting a fixture; never use broad process matching.

### reftable (#904): defer migration

A newly initialized reftable fixture was readable by both installed clients,
and Git verified its recovery bundle. This is only a capability/recovery
smoke. It does not establish an end-to-end gain, old-client refusal, IDE
support, recovery coverage, or a safe existing-repository migration.
The current submit diagnostic instead found repeated Git subprocess work.
Keep existing files-backend repositories unchanged. A future benchmark
needs controlled ref counts, lookup/enumeration/fetch/worktree distributions
and a tested rollback. Do not enable the unrelated experimental bundle.

### worktreeConfig (#906): defer a broker migration

Sparse checkout is a concrete consumer: Git enabled worktreeConfig in the
disposable sparse fixture, while its primary checkout remained full.
There is no demonstrated broker setting that requires a production
extension migration today. Command-local overrides remain preferable where
they suffice. Broker effective push-configuration replay explicitly excludes
repository-context keys (`git.rs::is_repository_context_key`), so copying
effective configuration into another repository is not a migration plan.
Enabling the extension needs explicit handling for primary and existing
linked checkouts, core.worktree/core.bare, interrupted states and every
supported client. The two recent clients do not qualify that matrix.

### sparse checkout (#907): reject as a default; keep a future opt-in prototype

The cone-mode fixture omitted a tracked file deliberately required by a
synthetic gate. Its sibling primary checkout retained the file. Disabling
sparse checkout restored it. This proves isolation and a full-checkout
fallback for this fixture; it also demonstrates why a successful Git
checkout is insufficient evidence that an agent or gate has complete input.

Full-tree consumers include the workspace manifests and crate reverse
dependencies selected by `scripts/test-like-ci.sh`, repository policy and
preparation declarations, graph-integrity fragment manifests, documentation
checks and generated skill/template parity. A package-only pattern cannot
be assumed to cover their inputs. Current experiments do not qualify these
consumers under sparse operation. Do not expose a production broker option
until omitted inputs cause an explicit unsupported result, task/branch
changes reconcile patterns, valuable ignored files survive transitions,
and full-checkout recovery is tested. No agent-context or gate pass claim
is made from the Git smoke.

## Worktree/dependency reuse (#903)

This Git-only experiment supplies no Rust/Node cold/warm preparation result.
That issue remains open. Continue a session when its source and preparation
digest remain suitable; start or reuse through the broker so freshness and
preparation are revalidated. Share immutable package stores and managed cache
classes through existing policy, not writable node_modules or mutable
worktrees between agents. A resource-bounded Rust and Node/Python comparison
belongs in the combined qualification phase, with disk/inode counts and
cache-hit provenance reported separately from checkout time.
