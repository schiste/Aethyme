# Git options for broker worktrees

Last Updated: 2026-10-25

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

## Explicit linked-worktree workloads

The runner also accepts bounded `--files` (12..50000), `--samples` (1..21),
and `--linked` parameters. Defaults retain the original 1,800-file,
seven-sample primary-checkout workload. For example:

```sh
python3 packages/aethyme-eval/benchmarks/performance/git_worktree_options.py \
  --fixture-parent /safe/local/scratch --output /safe/local/new-report \
  --files 10000 --samples 7 --linked
```

The caller chooses an existing scratch parent on the filesystem being measured;
the runner creates only a new synthetic child and does not enroll that parent.
The report records topology, actual file count and repetitions. Linked mode
records repeated detached checkout creation, resolves index paths through Git,
and asserts that editing the linked checkout leaves its primary clean. Each
variant has its own repository configuration, enabled before checkout timing.

A filesystem monitor is stopped and its stopped state verified for each temporary
checkout before removal, including those inside the timing loop. Final cleanup
also checks the exact primary and linked checkouts. First observations remain
warmth-unknown; checkout samples include ordinary Git setup costs and do not
establish cold-cache or broker/gate performance. Interrupted runs retain command
receipts and their fixture path rather than producing a successful report.

## Matching 10,000-file linked runs

Two sequential macOS 27.2 APFS runs used Git 2.55.0 with Apple Git 2.54.0 as
an alternate reader/writer: one on the internal Apple Fabric volume and one on
an external USB volume. Each had 10,000 tracked files, seven samples per status
state and seven detached checkout-creation samples. Untracked/ignored controls,
alternate-client index writes, primary-checkout isolation and monitor teardown
passed. Both complete fixtures were removed. The interrupted earlier external
attempt is excluded from these results.

Status values below are median / maximum milliseconds, not cold-cache or p99
measurements. Variants ran sequentially; host and cache order effects remain.

| Volume / variant | Clean | Dirty | Untracked + ignored | Checkout creation |
|---|---:|---:|---:|---:|
| Internal / baseline | 36.19 / 46.58 | 36.74 / 43.01 | 35.11 / 48.42 | 1535.35 / 2600.98 |
| Internal / manyFiles | 91.31 / 126.65 | 24.85 / 39.14 | 27.59 / 31.76 | 1549.43 / 2319.64 |
| Internal / fsmonitor | 38.37 / 65.85 | 33.32 / 41.93 | 29.75 / 69.88 | 1521.25 / 2438.81 |
| External / baseline | 49.23 / 58.52 | 49.16 / 54.92 | 34.84 / 42.96 | 2588.72 / 4758.07 |
| External / manyFiles | 81.90 / 108.15 | 29.35 / 71.42 | 26.81 / 30.06 | 2315.69 / 3715.22 |
| External / fsmonitor | 44.76 / 50.10 | 41.36 / 50.27 | 30.36 / 74.18 | 2096.40 / 3932.73 |

The baseline index was 880,540 bytes on both volumes; manyFiles reduced it to
704,252 / 704,341 bytes (internal / external). These runs extend filesystem and
linked-checkout coverage, but do not qualify older supported clients, IDEs,
network filesystems, monitor CPU/RSS, controlled cold caches, or actual broker
and gate latency. The mixed clean-state and tail results support keeping both
options opt-in. No existing repository setting was changed.

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

The [ref-heavy follow-up](git-ref-backend-assessment.md) now measures 2,000
agent refs on internal and external APFS with both installed clients. Ref
enumeration improved against loose refs, while packing files refs largely closed
the gap; lookup and worktree results were mixed. Exact branch-ref,
alternate-reader, local-fetch, bundle-to-files restoration and non-concurrent
conversion round trips passed. These are Git-only observations, not broker,
minimum-client, IDE or interrupted-migration qualification. Keep existing
files-backend repositories unchanged and do not enable the experimental bundle.
The runtime still declares Git 2.38; the two recent clients do not qualify it.

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
