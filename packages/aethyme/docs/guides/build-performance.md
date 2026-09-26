# Build Performance

Last Updated: 2026-09-26

This guide covers two things that make local Rust builds cheaper. The first is
a repository setting, dev debug-info trimming, which applies to everyone. The
second is sccache, which each developer opts into on their own machine. CI uses
neither sccache nor any machine-level setting.

## 1. Dev debug info (repository, everyone)

The repository-root `.cargo/config.toml` sets:

```toml
[profile.dev]
debug = "line-tables-only"   # file:line backtraces for workspace crates

[profile.dev.package."*"]
debug = false                 # no debug info for third-party crates
```

- `profile.test` inherits from `profile.dev`, so `cargo test` gets the same
  settings. The release profile is unchanged.
- Cargo reads this file for every command run at or below the repository
  root, including `--manifest-path packages/aethyme/rust/Cargo.toml` from the
  root.
- Profiles in a config file **override** the same keys in a `Cargo.toml`
  `[profile]` table (Cargo reference, "Configuration", `[profile]`). The
  workspace manifest defines no profiles. If you need different dev debug
  info, change it here, not in the manifest.
- `CARGO_PROFILE_DEV_DEBUG` / `CARGO_PROFILE_TEST_DEBUG` environment variables
  override the profile-level key. The gates set them to `1`. The
  `package."*"` override still applies there, so gate dependencies are built
  without debug info.

To confirm the setting applies, run
`cargo build -v -p aethyme-graph-schema`. Workspace crates show
`-C debuginfo=line-tables-only`, and dependencies show no `-C debuginfo` flag.

**Backtraces.** A panicking test with `RUST_BACKTRACE=1` still reports
`src/…rs:line:col` for every workspace frame. Dependency frames keep their
symbol names but lose file:line. If you need to step through a dependency in a
debugger, override it locally for that session:
`CARGO_PROFILE_DEV_PACKAGE_<NAME>_DEBUG=2`, or
`CARGO_PROFILE_DEV_DEBUG=2` for workspace crates.

## 2. sccache (per developer, optional)

sccache caches compiled crates across every build on a machine, so a new
session worktree, a broker gate cache, or an eval build reuses dependencies
compiled elsewhere.

### Opt in

```bash
brew install sccache
```

Create `~/.cargo/config.toml`, the machine-wide Cargo config (not the
repository's):

```toml
[build]
rustc-wrapper = "/opt/homebrew/bin/sccache"
```

Use the absolute path, so that a caller running cargo with a reduced `PATH`
still finds the wrapper. Never put `rustc-wrapper` in the repository's
`.cargo/config.toml`: contributors and CI without sccache would fail with
`could not execute process`.

To opt out, delete that file, or run a single command with `RUSTC_WRAPPER=`
(empty) set.

### Cache cap: 10 GiB

The cap is set in sccache's own config file,
`~/Library/Application Support/Mozilla.sccache/config` on macOS, rather than
through `SCCACHE_CACHE_SIZE`. The file is read by the sccache server
regardless of which shell, agent, or broker process started it. An
environment variable would have to be exported everywhere.

```toml
[cache.disk]
size = 10737418240   # 10 GiB; least-recently-used entries are evicted
```

The cache lives in `~/Library/Caches/Mozilla.sccache`. After you edit the
config, restart the server with `sccache --stop-server`. The next build starts
it again.

**Disk headroom.** Broker gates refuse to run below 8 GiB free. A full 10 GiB
cache on a nearly full disk can cause that. Lower `size` if your disk runs
close to the limit.

### Check it

```bash
sccache --show-stats      # requests, hits/misses, hit rate, cache size, max size
sccache --zero-stats      # reset counters before a measurement
```

### Clear it

```bash
sccache --stop-server
rm -rf ~/Library/Caches/Mozilla.sccache
```

### What gets cached

- **Dependencies.** Registry crates build from the same
  `~/.cargo/registry/src` paths in every worktree, so they hit across
  worktrees. This is the main win.
- **Workspace crates in normal dev builds are not cached.** They build with
  incremental compilation, which sccache passes through. Keep incremental
  enabled: it is what makes edit-rebuild cycles fast.
- **Gates** export `CARGO_INCREMENTAL=0`, so their workspace crates are
  cacheable too. Hits across different worktree paths are not expected,
  because the source path is part of the key.
- Binaries, tests, proc-macros and build scripts (`crate-type` bin, dylib,
  proc-macro) are never cached. Linking is never cached.

### Gates use it

The broker's gate runner (`aethyme-broker/src/gates.rs`) spawns `sh -c
<command>` with the inherited environment. It adds its own variables and, at
most, removes wrapper directories from `PATH`. `HOME` is untouched, so Cargo
reads `~/.cargo/config.toml` inside gates. Because the wrapper path is
absolute, the `PATH` rewrite cannot hide it. Verified on 2026-09-26: during
`aethyme broker advanced gates run`, `sccache … rustc` processes ran with
`--out-dir ~/Library/Caches/Aethyme/gates/<key>/rust/…`, and the server's
compile-request count rose by about 210.

## Measurements (2026-09-26)

These were taken on one commit (`2ff06a98` plus the profile change), with
rustc 1.96.0 on a 10-core Mac. They are **not** from an idle machine: other
sessions' builds and gates held the load average between 20 and 70
throughout, so wall times carry roughly 2x noise. Read sizes and hit rates as
the signal and times as indicative only. Command:
`cargo build --workspace --all-targets` from `packages/aethyme/rust`.

| Configuration | Clean build | Incremental (touch one engine file) | Clean `test --no-run` | `target/` | sccache hits |
|---|---|---|---|---|---|
| A. Baseline, internal disk (load 10 to 35) | 106s | 67s | 194s | 3.3G | n/a |
| B. Trimmed, internal disk (load 29 to 71) | 150s | 191s | 455s | 3.0G | n/a |
| B'. Trimmed, external exFAT disk (load 21 to 57) | 172s | 133s | 156s | 3.9G | n/a |
| C. Trimmed + sccache, cold cache (exFAT) | 123s | 67s | n/a | 3.9G | 0 / 210 |
| C. Trimmed + sccache, warm cache (exFAT) | 98s | 58s | 137s | 3.9G | 210 / 210 (100%) |

- Debug trimming saved about 10% of `target/` (3.3G to 3.0G). Much of
  `target/` is incremental state and statically linked test binaries, which
  trimming does not shrink much.
- On the same disk under similar load, a warm sccache cut the clean build by
  about 20% (123s to 98s cold to warm). The rest is workspace crates and
  linking, which sccache does not cache in dev builds. A second cold run under
  load 60 took 256s, which shows how large the noise was.
- The cache for this workspace's dependencies was about 300 MB.
- C and B' ran on an external exFAT volume because the internal disk had under
  10 GiB free. exFAT copies files where APFS hard-links them, which inflates
  `target/`.
- One of three sccache runs failed an incremental rebuild with
  ``crate `hashbrown` required to be available in rlib format``. The failure
  did not reproduce on either retry. If you see it, `cargo clean -p <crate>`
  or `RUSTC_WRAPPER= cargo …` works around it.
