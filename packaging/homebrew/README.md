# Aethyme Homebrew tap source

This directory is the reviewed source for `schiste/homebrew-tap`. The tap
formula installs `aethyme` and `aethyme-engine-cli` from the same immutable
platform archive.

`Formula/aethyme.rb` is initially pinned to the already-published v0.1.5
archives. Their contents and GitHub-recorded SHA-256 values were independently
verified on 2026-08-23. Starting with v0.2.0, the release workflow renders the
formula from the signed Aethyme release manifest and publishes it as a release
asset for review before the tap is updated.

Install directly, trusting only this formula:

```bash
brew install schiste/tap/aethyme
```

After installation, normal Homebrew update flows apply:

```bash
brew update
brew upgrade aethyme
```

A third-party tap is executable Ruby maintained outside `homebrew/core`.
Direct formula installation is the narrower trust choice. Do not run
`brew trust --tap schiste/tap` unless every formula and command in that tap is
within the user's trust boundary.

## Publishing a stable update

The `homebrew-tap` job in `.github/workflows/release.yml` does this
automatically for every stable tag (tags containing `-` are skipped): after the
GitHub release is created it downloads the rendered `aethyme.rb` release asset,
checks that it points at that release's archives, and commits it to
`Formula/aethyme.rb` on the tap's `main` as `chore: update Aethyme to vX.Y.Z`.
It needs the repository secret `HOMEBREW_TAP_TOKEN`: a fine-grained personal
access token scoped to `schiste/homebrew-tap` only, with repository permission
Contents: Read and write (Metadata: Read-only is implied). Without the secret
the job emits a warning and succeeds without touching the tap. That push
bypasses the maintainer's local broker enrollment of the tap, which is accepted
for a formula-only bump.

The formula covers macOS (arm64, x86-64) and glibc Linux (arm64, x86-64). The
musl archive is for the installer only; Homebrew on Linux requires glibc.

To publish by hand instead (for example when the secret is absent):

1. Download `aethyme.rb`, `release-manifest.json`, and its Sigstore bundle from
   the stable release.
2. Verify the signed manifest and compare the reviewed formula to the generated
   release asset.
3. Replace `Formula/aethyme.rb` in `schiste/homebrew-tap` in a pull request.
4. Run `brew style`, `brew audit --strict`, install, and `brew test aethyme` on
   the supported Homebrew runners.
5. Merge only the reviewed commit SHA.

Preview releases never update the stable formula because Homebrew exposes one
current version per formula.

