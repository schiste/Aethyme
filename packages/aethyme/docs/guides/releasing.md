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
7. For a stable release, the workflow verifies the signed manifest, renders
   `aethyme.rb` from its exact tag and source commit, and publishes it to the
   tap through an Aethyme broker session. It checks the tap's default branch
   and current formula blob SHA before the write, then reads the formula back
   byte-for-byte and reports the resulting commit. The workflow needs the
   fine-grained `HOMEBREW_TAP_TOKEN` secret with write access to
   `schiste/homebrew-tap` only.

   If that secret is unavailable, run the repository-owned publisher from an
   active Aethyme broker worktree after verifying the release manifest and
   downloading its `aethyme.rb` asset. Set `REF_NAME` to the stable tag and
   `SESSION_ID` to that worktree's broker session:

   ```sh
   tap_branch=$(gh api repos/schiste/homebrew-tap --jq .default_branch)
   tap_sha=$(gh api "repos/schiste/homebrew-tap/contents/Formula/aethyme.rb?ref=$tap_branch" --jq .sha)
   scripts/publish-homebrew-tap.sh --formula dist/aethyme.rb --tag "$REF_NAME" \
     --release-repo schiste/Aethyme --tap-repo schiste/homebrew-tap \
     --branch "$tap_branch" --expected-file-sha "$tap_sha" --dry-run
   scripts/publish-homebrew-tap.sh --formula dist/aethyme.rb --tag "$REF_NAME" \
     --release-repo schiste/Aethyme --tap-repo schiste/homebrew-tap \
     --branch "$tap_branch" --expected-file-sha "$tap_sha" --session "$SESSION_ID"
   ```

   The publisher uses OpenSSL's portable base64 mode, refuses a stale SHA or
   non-default branch before creating a broker operation, and verifies the
   exact remote formula and commit after the write.
8. Close release-bound issues only after the published and installed artifacts
   have passed those checks.

The router and engine sibling are one release unit. Never publish, install, or
roll back one without the other.
