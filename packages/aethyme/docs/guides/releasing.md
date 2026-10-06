# Releasing Aethyme

Last Updated: 2026-09-24

This is the maintainer contract for choosing and publishing Aethyme versions.

## Version authority

The default release is a patch release: increment only the third component,
`x.x.y` → `x.x.(y+1)`. Agents and automated release work may perform that
patch increment when publication is authorized.

Changing either of the first two components is reserved to the maintainer.
Do not infer authority for an `x.(x+1).0` or `(x+1).0.0` release from a request
to fix, publish, deploy, or cut a new version. Such a release requires an
explicit version choice from the maintainer.

SemVer impact and publication authority are separate. If a change appears to
require a minor or major increment but none was explicitly authorized, stop
before versioning and report the compatibility concern. Do not silently widen
the requested version change.

## Patch release checklist

1. Integrate each implementation series independently through the broker.
2. Update the workspace version, lockfile, the `.aethyme/engine-version` pin,
   and `CHANGELOG.md`: rename `[Unreleased]` to `## [X.Y.Z] - <date>`. If the
   release migrates the broker database one way, removes or changes a command,
   flag, exit code, or output contract, or adds an install requirement, it is
   breaking: add a `## vX.Y.Z` section to the top-level `UPGRADING.md` (with
   `### Compatibility`, `### Migrate and verify`, and `### Rollback`) and open
   the CHANGELOG entry with a `**Breaking:**` line that links
   `UPGRADING.md#vXYZ` (the version without dots). Nothing else is
   per-release: the workflow renders the GitHub release body from those two
   files, and `cargo test -p aethyme-testkit --test release_contract` renders
   it for the workspace version and checks the pairing for every release.
3. Stage every new file, then redeploy the enhancement from a binary built at
   the new version:

   ```bash
   git add -A
   (cd packages/aethyme/rust && cargo build -p aethyme-cli -p aethyme-engine)
   ./packages/aethyme/rust/target/debug/aethyme deploy --repo "$PWD"
   git add -A
   ```

   Both halves of that order matter. The stamp is compiled in through
   `env!("CARGO_PKG_VERSION")`, so a deploy run from the installed binary
   records the *previous* version. And the generated onboarding freshness digest
   counts **tracked** files, so a deploy run before `git add` omits any file
   this release just created and records a digest one file short. It is
   self-correcting at the next deploy, which is exactly why it survives review:
   nothing fails, and the committed digest is quietly wrong until someone
   redeploys.
4. Run `cargo fmt --check --all`, `cargo test --workspace`, release contract
   checks, and `git diff --check`.
5. Submit the release series, inspect `broker advanced ship plan`, and execute only the
   exact confirmed integration SHA.
6. Create and push the matching annotated tag through the coordinated Git
   lane. Wait for the release workflow and verify the signed manifest,
   checksums, installer, and every supported archive.
7. For a stable release, `release.yml` calls `homebrew-tap.yml`, which
   verifies the signed manifest, renders `aethyme.rb` from the tag's exact
   source commit, and checks that this matches the release asset. It then
   writes `Formula/aethyme.rb` to the tap's default branch with one Contents
   API call that carries the file's current blob SHA, so GitHub refuses it if
   the formula changed in between, and reads the result back byte-for-byte.
   The write needs the `HOMEBREW_TAP_TOKEN` secret (fine-grained, contents
   write on `schiste/homebrew-tap` only).

   To retry, or after adding a missing token, re-run the Homebrew tap
   workflow for the tag (`workflow_dispatch` with input `tag`). An agent
   runs `aethyme broker advanced gh --session <id> --repo schiste/Aethyme
   --reason "<authorization>" -- workflow run homebrew-tap.yml -f
   tag=vX.Y.Z`. Republishing the same formula is a no-op. Never write the
   tap from a workstation: `scripts/publish-homebrew-tap.sh` refuses writes
   outside GitHub Actions, and with `--dry-run` it only checks the tap.

   The write deliberately bypasses the Aethyme broker. A runner's broker
   database is empty, so a session there would coordinate with nothing; the
   blob-SHA precondition and the read-back are what make the write safe.
   Pull requests that touch the publication path rehearse it against the
   latest stable release and the live tap without writing.
8. Install the published pair on an installer-managed machine with
   `aethyme self-update --version X.Y.Z` (the same as `aethyme update apply`).
   It verifies the signed manifest whenever cosign is on PATH
   (`--require-signature` makes that mandatory), checks the archive against
   the manifest's SHA-256, refuses a pair whose binaries do not both report
   X.Y.Z, and switches router and engine together. Then confirm that
   `aethyme --version` and `aethyme-engine-cli --version` report the same
   version and build commit before collecting performance telemetry (#251).
   Cargo and Homebrew installs keep their own update commands, which
   `aethyme update plan` names.
9. Close release-bound issues only after the published and installed artifacts
   have passed those checks.

The router and engine sibling are one release unit. Never publish, install, or
roll back one without the other.
