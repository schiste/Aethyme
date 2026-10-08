# Internal dogfooding install and recovery guide (protocol v2)

Last Updated: 2026-10-08

For maintainers and internal teammates setting up Aethyme in a project-owned
repository or disposable Playground repository. Select one supported release
and keep both binary versions and repository gate policy stable during an
observation window. Historical v0.8.3/v0.8.4 spellings below are compatibility
notes only; use the current command forms for a current release.

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
For a fixed internal observation window, use Homebrew only when the tap
formula is pinned to the selected version; otherwise use the versioned
installer or matching source tag below. Confirm both installed binaries report
that same version.

**Installer script** (no Homebrew). It downloads the release archive for
your platform and checks its checksum:

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
aethyme --version
aethyme-engine-cli --version
```

For an exact internal version pin, set `PILOT_AETHYME_VERSION` to the
selected release and pass it to the installer:

```bash
: "${PILOT_AETHYME_VERSION:?Set the version selected for this observation window}"
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh \
  | sh -s -- --version "$PILOT_AETHYME_VERSION"
```

For a signature-verified install, download `install.sh`, read it, and run it
with `--verify-signature` (needs Cosign 3). Review updates explicitly with
`aethyme update check`, `aethyme update plan` and
`aethyme update execute --confirm <manifest-sha256>`; nothing updates in the
background. A failed download, validation, staged smoke test, or activation
check restores the prior installer-managed bundle automatically. There is no
user-facing command to manually switch back to that bundle; do not claim a
manual rollback path unless a reviewed procedure has been exercised.

**From source** (needs a Rust toolchain; check out the release tag selected
for the observation window):

```bash
git clone --branch "v$PILOT_AETHYME_VERSION" --depth 1 https://github.com/schiste/Aethyme.git
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
aethyme broker start --task "Fix the flaky login test" --short-name "Login test" --agent "Your Name <you@example.com>"
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
creating a new one: `aethyme broker start --adopt --task "..." --short-name "..."`
(v0.8.3: `aethyme broker adopt --task "..." --short-name "..."`).

## 6. Start the metrics export

```bash
./export-metrics.sh --repo /path/to/your-repo --label app
```

Then add it to cron; see [metrics-export.md](metrics-export.md).

## Historical v0.8.3 and v0.8.4 spellings

This table is for reading old command notes, not for installing a current release.
The documented alias window ended at v0.8.8; use the current spellings above
for later releases.

<!-- deprecated-spellings: begin (old spellings named on purpose; see aethyme-testkit/tests/deprecated_spelling_callers.rs) -->

| v0.8.4 | v0.8.3 |
| --- | --- |
| `aethyme broker start --adopt` | `aethyme broker adopt` |
| `aethyme broker start --reuse` | `aethyme broker adopt --reuse` |
| `aethyme broker unblock` (no id: list blockers) | `aethyme broker blockers` |
| `aethyme broker advanced trust` | `aethyme broker trust` |
| `aethyme broker advanced leases claim` | `aethyme broker leases claim` |
| `aethyme broker advanced quick-test` | `aethyme broker quick-test` |

<!-- deprecated-spellings: end -->

`init`, `certify`, `deploy`, `start --task`, `status`, `submit`, `finish`,
`unblock <id>` and `gc plan|apply` are the same in both historical releases.
