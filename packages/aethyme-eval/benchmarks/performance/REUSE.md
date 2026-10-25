# Broker worktree reuse diagnostic

Updated: 2026-10-25

`broker_reuse.py` measures a fixed sequence: fresh initial checkout, fresh
checkout with warmed managed stores, continuation (sync), and `start --reuse`.
Each path records dependency preparation and one explicitly selected gate.
It is opt-in, diagnostic only, and never compiles Aethyme. Default invocation
prints a plan and starts no child process. `--run` **can execute expensive
preparation and gates** from the supplied fixture. Review them before execution.
Do not put it in ordinary PR tests.

The harness uses the concurrent-submit diagnostic's bounded process supervisor,
private environment, command receipts and local transport shims. Every command
has a 900-second deadline (configurable within 1–1,800 seconds), bounded output,
and owned-group settlement. These bounds do not constrain detached descendants
or provide an OS sandbox. Recipes must be trusted, synthetic and finite. Native
workloads must use the host admission declared by their preparation/gate policy.

## Stage fixtures

Use independently reviewed **synthetic Playground** repositories, never the
Aethyme repository or another real project. Supply a Rust-heavy fixture and a
Node or Python application fixture with pinned dependencies. A tiny std-only
crate or an empty virtual environment is not representative. Record dependency
counts, workload shape and omissions in `workload_description`; a profile name
alone does not establish representativeness. Keep mutable targets, virtual
environments and dependency trees local to each worktree. Only immutable
package stores may be shared through the product's preparation policy.

For each independent sample create a new absolute fixture root, separate from
both the source checkout and the report output:

```
Playground/
  PLAYGROUND.json
  repo/           # ordinary clean synthetic checkout, main at the pinned commit
  remote.git/     # local bare origin containing that same main commit
  home/           # empty private home
  tmp/            # empty private temporary directory
```

The seed commit must contain `.aethyme/prepare.toml`, an explicit bounded
`.aethyme/gates.toml`, dependency lockfiles and ignores for generated state.
Set `[cleanup] auto_remove = false` so the fresh comparison checkout remains.
Configure the checkout's origin as `git@github.com:playground/reuse.git`;
the staged SSH shim serves only `remote.git`, and `gh` supports only read-only
empty PR discovery. No actual GitHub repository is required. Do not add Git
includes, URL rewrites, external SSH commands or filesystem monitors.

Example declaration (replace all pins with the fixture's actual values):

```json
{
  "schema_version": 1,
  "synthetic": true,
  "profile": "rust-heavy",
  "workload_description": "Describe the reviewed dependency graph, application workload and known representativeness limits here.",
  "baseline": "0000000000000000000000000000000000000000",
  "gate": "verify",
  "lockfiles": {"Cargo.lock": "0000000000000000000000000000000000000000000000000000000000000000"},
  "toolchain": {"bin": "/absolute/toolchain/bin", "rustup_home": "/absolute/readable/rustup"}
}
```

Other profile labels: `python-service`, `node-service`. `toolchain` is optional;
only its two documented directory selectors are accepted. Package stores are
isolated with fixture-owned Cargo/npm/pip locations, independent of toolchain
binaries. Recipes must not override these with host stores. Freeze and retain
the declaration, source commit/bundle, lockfiles and toolchain identity before
running; immutable Git commit identity does not recover missing source bytes.

Stage the reviewed transport scripts without running any child command:

```
python3 broker_reuse.py --binary /pinned/bin/aethyme \
  --fixture-root /absolute/Playground --stage-shims
```

This prints the exact isolated environment and human trust argv. A human must
review and approve that synthetic repository's gate policy using **that
environment**. The harness neither grants trust nor uses test-only trust bypasses.
The initial managed preparation/cache/worktree directories must remain empty.
Human approval does not authorize launching models, network providers or unrelated
commands. Review preparation and gates separately.

## Measure and retain

```
python3 broker_reuse.py --binary /pinned/bin/aethyme \
  --fixture-root /absolute/Playground --output /new/durable/report --run
```

The fixture and output must not overlap. CLI/engine bytes and the three-file harness helper closure are hashed before
execution and rechecked afterwards. The source baseline, Git common directory,
origin and lockfile pins are verified; each measured checkout is rechecked
before and after its phase, and gate evidence must name the frozen source tree. A retained exclusive attempt record
prevents treating a partial run as a fresh cold sample. On failure inspect all
receipts; create a new fixture for a new sample. No automatic retries, forced
cleanup, source publication, notifications or provider calls are added.

`commands.json` preserves argv, complete bounded stdout/stderr, process status
and command wall intervals. Intervals include spawn, output drain and owned-group
settlement. `cpu.jsonl` records sequential child user/system CPU deltas, including
failed commands; it is not host CPU telemetry. `report.json` keeps each phase's
session/worktree identity, preparation step counts/digest, actual gate cache
flags/provenance, and storage snapshots/deltas. Inventory time is outside command
intervals. Storage counts logical bytes per filename, allocated blocks once per
file inode, file names, unique file inodes, directories and symlinks. It does not
measure APFS snapshot/cloning attribution, system-wide reclaimed space or volumes
outside the fixture. A symlink is inventoried without following its target.

Retain at least 8 GiB free before each child command. Inventory refuses above
16 GiB or 500,000 names; this is a between-command check, not a filesystem quota.
Keep fixtures for review; reclaim their exact roots only after checking process
settlement, broker ownership and retention of reports. The harness never deletes
a user-supplied fixture automatically.

The initial/warm order refers to managed stores; OS page caches are uncontrolled.
Cache hits are observed, not assumed. Repeat independently on both workload
families and report raw samples before medians or savings. The matrix does not
yet cover stale/conflicting sessions, external minimum toolchains or interrupted
recovery. Therefore this implementation alone does not satisfy all of #903.

Offline controls (no Aethyme/native calls or dependencies):

```
python3 -B -m unittest discover -s . -p test_broker_reuse.py -v
```
