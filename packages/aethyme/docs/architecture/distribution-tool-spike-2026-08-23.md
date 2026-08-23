# Distribution Tool Spike

Last Updated: 2026-08-23

## Decision

Do not replace Aethyme's release workflow with `cargo-dist` 0.32.0 before
v0.2.0. Keep the existing release-manifest generator and paired archive
builder as the release authority.

Re-evaluate `cargo-dist` after either:

1. `aethyme` and `aethyme-engine-cli` are produced by one Cargo package; or
2. the engine daemon becomes an internal mode of the `aethyme` executable.

This is a bounded compatibility decision, not a permanent rejection of the
tool.

## Constraint under test

Aethyme is one product delivered as two cooperating executables:

- `aethyme`, the router and public CLI;
- `aethyme-engine-cli`, the engine-daemon sibling.

A release is valid only when one platform archive contains both executables,
one archive digest covers the pair, both embedded versions match, and their
engine protocol is compatible. An installer or package manager must never be
able to update only one member of the pair.

## Findings

The current `cargo-dist` documentation gives a precise package boundary:

- multiple binaries in one Cargo package are one application and are bundled
  in every archive and installer for that application;
- binary-producing packages in a workspace are separate applications with
  independent archives and installers;
- a unified release tag can announce several same-version applications, but
  it does not combine their installation units;
- its Homebrew publisher installs the prebuilt archive for an application and
  automatically updates a configured tap;
- archive checksums and a unified checksum file are supported, and Homebrew
  formulae always embed SHA-256 values;
- GitHub artifact attestations, CycloneDX SBOMs, and auditable binaries are
  available supply-chain options;
- the bundled self-updater is explicitly experimental and automatically
  performs an upgrade without Aethyme's required review-digest confirmation.

Aethyme currently produces the router from `aethyme-cli` and the sibling from
`aethyme-engine`. Under `cargo-dist`'s documented model they are two
applications. Lockstep workspace versions and a unified tag would still yield
independent install artifacts, so the tool cannot presently enforce the
paired-install invariant.

## Options considered

| Option | Pair invariant | Result |
| --- | --- | --- |
| Adopt `cargo-dist` with the current packages | No: separate packages are separate applications | Rejected |
| Move the engine binary into `aethyme-cli` only for packaging | Yes, but creates a misleading ownership boundary and duplicate build wiring | Rejected |
| Add a custom post-build combiner around `cargo-dist` | Possible, but the custom combiner again becomes the real release authority | Rejected |
| Keep the current paired archive and manifest | Yes, already tested on every release target | Selected |
| Internalize the daemon in `aethyme` | Removes the skew hazard entirely | Long-term target |

## Selective adoption

The release workflow should adopt useful outputs without handing over the
artifact boundary:

1. retain the signed Aethyme release manifest as the compatibility authority;
2. publish standalone and unified SHA-256 checksum assets;
3. keep GitHub/Sigstore provenance for the manifest and consider artifact
   attestations for the archives;
4. evaluate CycloneDX and `cargo-auditable` after the v0.2.0 delivery path is
   stable;
5. generate the Homebrew formula from Aethyme's signed manifest so the formula
   and native updater consume the same archive metadata.

## Homebrew consequence

The tap formula must select one platform archive and install both executables
from that archive. It must never depend on the independently published Cargo
packages. Stable releases update the formula; preview releases do not replace
the stable Homebrew version because Homebrew exposes one current version per
formula.

The recommended user command is `brew install schiste/tap/aethyme`. Direct
formula installation trusts only that formula; trusting the whole third-party
tap is a broader choice and must be documented as such. Once tapped, normal
`brew update` and `brew upgrade aethyme` flows apply.

## Sources

- [cargo-dist complex workspace model](https://axodotdev.github.io/cargo-dist/book/workspaces/workspace-guide.html)
- [cargo-dist archive contract](https://axodotdev.github.io/cargo-dist/book/artifacts/archives.html)
- [cargo-dist checksum behavior](https://axodotdev.github.io/cargo-dist/book/artifacts/checksums.html)
- [cargo-dist Homebrew publisher](https://axodotdev.github.io/cargo-dist/book/installers/homebrew.html)
- [cargo-dist experimental updater](https://axodotdev.github.io/cargo-dist/book/installers/updater.html)
- [cargo-dist supply-chain features](https://axodotdev.github.io/cargo-dist/book/supplychain-security/index.html)
- [Homebrew tap creation and trust model](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap)

