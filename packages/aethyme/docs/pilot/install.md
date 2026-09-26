# Pilot install guide

Last Updated: 2026-09-26

From a fresh machine to a first submitted session. Commands use the v0.8.4
spellings; where v0.8.3 differs, the old spelling is noted and still works
(it prints a deprecation warning on v0.8.4 and later).

## 1. Install

Every install method gives you two binaries, `aethyme` and its required
sibling `aethyme-engine-cli`, which must report the same version.

**Homebrew** (macOS and Linux, recommended):

```bash
brew install schiste/tap/aethyme
aethyme --version
aethyme-engine-cli --version
```

Update later with `brew update && brew upgrade aethyme`.

**Installer script** (no Homebrew). It downloads the release archive for
your platform and checks its checksum:

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
aethyme --version
aethyme-engine-cli --version
```

To pin the pilot release, pass it: `sh -s -- --version 0.8.4`. For a
signature-verified install, download `install.sh`, read it, and run it with
`--verify-signature` (needs Cosign 3). Review updates explicitly with
`aethyme update check`, `aethyme update plan` and
`aethyme update execute --confirm <manifest-sha256>`; nothing updates in the
background.

**From source** (needs a Rust toolchain):

```bash
git clone https://github.com/schiste/Aethyme.git
cd Aethyme
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

`--locked` is required. Without it Cargo re-resolves dependencies and the
build fails with `error[E0080]` in a dependency, which looks like a
toolchain problem but is not.

## 2. Enroll a repository

Run these from the root of the repository your agents work on, on a clean
checkout of its default branch.

```bash
cd /path/to/your-repo
aethyme init
```

`aethyme init` does three things and is safe to run twice: it certifies the
repository (read-only checks), scaffolds the broker's configuration and
database under `.aethyme/`, and, if you have none, drafts
`.aethyme/gates.toml` from your manifests. Gates are the checks that run on
the merged tree before work lands, for example your test and lint commands.
Open the draft, keep the checks you trust to be fast and reliable, and
commit it. `aethyme certify` re-runs the read-only checks at any time.

Optionally, deploy the agent guidance (`AGENTS.md`, `CLAUDE.md`, skills and
hooks that tell your agents to use the broker). Review the plan first, then
apply exactly that plan:

```bash
aethyme deploy plan --repo . --diff
aethyme deploy execute --repo . --confirm <plan-sha256>
aethyme deploy verify --repo .
```

Use `aethyme deploy --local-only --repo .` instead to try it without
committing anything shared.

## 3. Trust the gates

Gate and prepare commands come from files in the repository, and the broker
runs them as you. So nothing they declare runs until a person on this machine
has approved the exact policy. Until then `submit` refuses (exit 3) and names
the command to run.

```bash
aethyme broker advanced trust --repo .
aethyme broker advanced trust status --repo .
```

On v0.8.3 the spelling is `aethyme broker trust`. `trust` prints every gate
and prepare command, asks you to confirm, and records a digest of the policy.
It refuses when stdin is not a terminal: agents cannot approve their own
commands. When someone changes `gates.toml`, run it again.

## 4. Smoke test

```bash
aethyme broker advanced quick-test
```

This runs an adopt, commit and submit round trip in a disposable repository
and removes it afterwards (v0.8.3: `aethyme broker quick-test`).

## 5. The first session

```bash
aethyme broker status
aethyme broker start --task "Fix the flaky login test" --agent "Your Name <you@example.com>"
```

`start` creates an isolated worktree and a session, and prints the session id
and the worktree path. Point one agent at that worktree, let it edit and
commit, then:

```bash
aethyme broker submit --session <id>
aethyme broker finish --session <id>
```

`submit` merges the session onto the local `aethyme/integration` branch in a
simulation, runs the affected gates on the merged tree, and promotes only if
they pass. It never pushes. `finish` closes the session once nothing is left
unsubmitted. Note the time: your first successful `submit` is one of the pilot
metrics.

If an agent is already working in its own worktree, register it instead of
creating a new one: `aethyme broker start --adopt --task "..."`
(v0.8.3: `aethyme broker adopt --task "..."`).

## 6. Start the metrics export

```bash
./export-metrics.sh --repo /path/to/your-repo --label app
```

Then add it to cron; see [metrics-export.md](metrics-export.md).

## v0.8.3 and v0.8.4 spellings

| v0.8.4 | v0.8.3 |
| --- | --- |
| `aethyme broker start --adopt` | `aethyme broker adopt` |
| `aethyme broker start --reuse` | `aethyme broker adopt --reuse` |
| `aethyme broker unblock` (no id: list blockers) | `aethyme broker blockers` |
| `aethyme broker advanced trust` | `aethyme broker trust` |
| `aethyme broker advanced leases claim` | `aethyme broker leases claim` |
| `aethyme broker advanced quick-test` | `aethyme broker quick-test` |

`init`, `certify`, `deploy`, `start --task`, `status`, `submit`, `finish`,
`unblock <id>` and `gc plan|apply` are the same in both. The old spellings
keep working until v0.8.6.
