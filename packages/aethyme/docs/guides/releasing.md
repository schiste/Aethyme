# Releasing Aethyme

Last Updated: 2026-09-01

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
2. Update the workspace version, lockfile, changelog, workflow guide path, and
   version-specific upgrade guide to the chosen patch version.
3. Stage every new file, then redeploy the enhancement from a binary built at
   the new version:

   ```bash
   git add -A
   (cd packages/aethyme/rust && cargo build -p aethyme-cli -p aethyme-engine)
   ./packages/aethyme/rust/target/debug/aethyme enhance deploy --repo "$PWD"
   git add -A
   ```

   Both halves of that order matter. The stamp is compiled in through
   `env!("CARGO_PKG_VERSION")`, so a deploy run from the installed binary
   records the *previous* version. And the generated onboarding freshness digest
   counts **tracked** files, so a deploy run before `git add` omits the upgrade
   guide this release just created and records a digest one file short. It is
   self-correcting at the next deploy, which is exactly why it survives review:
   nothing fails, and the committed digest is quietly wrong until someone
   redeploys.
4. Run `cargo fmt --check --all`, `cargo test --workspace`, release contract
   checks, and `git diff --check`.
5. Submit the release series, inspect `broker ship plan`, and execute only the
   exact confirmed integration SHA.
6. Create and push the matching annotated tag through the coordinated Git
   lane. Wait for the release workflow and verify the manifest, checksums,
   installer, and every supported archive.
7. Update the Homebrew formula from the published archive and digest, then
   verify both installed binaries, `broker quick-test`, and repository
   enhancement compatibility.
8. Close release-bound issues only after the published and installed artifacts
   have passed those checks.

The router and engine sibling are one release unit. Never publish, install, or
roll back one without the other.
