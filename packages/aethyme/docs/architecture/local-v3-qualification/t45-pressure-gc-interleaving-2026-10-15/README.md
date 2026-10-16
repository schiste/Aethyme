# T45 pressure and GC interleaving

## Result

The frozen local T45 profile has one reviewed APFS positive and one reviewed Docker build-pressure positive. The APFS run observed real `ENOSPC` during both capture and hydration while each operation held archive shared-use protection and GC returned `ArchiveInUse`. The Docker run observed bounded tmpfs `ENOSPC` and bounded build-cgroup OOM while the same-store GC move held archive exclusive protection; it then reclaimed the named orphan and reconstructed the retained source. These results are scoped to the recorded images, limits, daemon, and test fixture.

The evidence payloads and byte counts are listed in [inventory.json](evidence/inventory.json). The 101 APFS files are copied byte-for-byte from the independently reviewed staging snapshot: 97 event records, one resource inventory, and three recorder files. Test logs and status files are also preserved byte-for-byte. The byte-exact APFS records and Docker or mutation command logs, along with the Docker profile summary and daemon-capacity receipt, contain absolute local paths, including the account name and worktree or temporary-volume paths. They are retained to preserve byte-exact run provenance. The package omits mutation runner and operator files containing runtime-gate values, private journals and database snapshots, full Docker inventories, and mixed Docker event streams; safe summaries bind their original hashes where available.

## Source and positive profiles

The APFS run is bound to source commit `4b5f3bc329d5f4108e3df62c67221a224c3c0710`, tree `8166dab40d2c419c67c5bae30c45dd4891c7e058`, with the test source hash recorded in `evidence/profile-summary.json`. It used an owned 512 MiB APFS image, 32 MiB source blob, 16 MiB journal floor, and 256 MiB normal reservation floor. The recorder completed with exit 0 and unchanged source. Independent review verified all 101 staged payloads, 97 event records, 41 read-only command pairs with exit 0, raw `ENOSPC`, lock witnesses, retained-source recovery, and owned cleanup.

The corrected Docker run is bound to commit `bc0b03d08d63080da4fd86a5afba8a4449428623`, tree `70290c6ac210217c05d14ddfa0350e3a4e0a4e10`, and `local_qualification.rs` SHA-256 `b3d6d3f1207aebe82bcce4812fecf582948d64ca5d8fff1d83d07c76cf53090b`. It used the pinned existing daemon endpoint and image in the profile summary. The test-only resource-pool ceiling is 3072 MiB; the build cgroup remains 64 MiB, output limit 1 MiB, and PID limit 32. Two simultaneous build reservations require 1288 MiB; four 644 MiB envelopes total 2576 MiB. The profile ceiling change does not alter the build pressure limits or assertions.

The completed Docker test passed once with exit 0. Its transcript records ENOSPC exit 28, OOM classification, no artifact for either pressure case, retained-source reconstruction, a same-store GC EX witness and named orphan reclamation, and the stable build remaining live. Independent review also verified the exact pinned daemon/image, read-only check exit codes, unchanged container list, and empty private host-state and recorder temporary inventories.

The successful Docker fixture removes its journals. The saved mixed global event stream is 146,458 bytes with SHA-256 `ba747429b978cadfab42ba93b2f165d403ba2b979f62602d689b32cd57581e39`; the duplicated full postflight snapshot is 162,400 bytes with SHA-256 `0dd364403b9efabee88ebeb1d8d433216012d09e00f650fd8be3b48e42030542`. Both remain private. Since the exact journal object-ID union is unavailable after cleanup, this package makes no owned-event count or global-event ownership claim for that positive.

## Mutation controls

All five reviewed mutations were restored exactly. Each produced the intended named guard failure with exit 101 and receives guard-negative credit only; none is another pressure-profile pass. M1 removes capture shared-use protection; M2 removes hydration shared-use protection; M3 weakens the GC archive lock; M4 completes GC before pressure; M5 moves the test observer after `run.finish`. The exact assertions, source hashes, recorder hashes, runner/wrapper pins, original attempt classifications, and correction-record hashes are in `evidence/mutation-summary.json`.

M1–M3 and M4 retain their original parser classifications in ignored `attempt.json` files; independent metadata-only corrections reclassify them as expected named failures from the raw transcript predicates. The public summaries preserve the original classifications and bind each correction hash. M3's post-invocation runner/wrapper check was derived after the attempt at `2026-10-15T15:08:08.772464Z`; it is not described as a contemporaneous check. M5 evidence records the exact `--exact` test filter, a `running 1 test` line, the sole named failure list, 0 passed/1 failed summary, exact timeout assertion, and exit 101. The runner does not separately count the test-start line.

## Non-credit attempts and limits

The initial 4096 MiB Docker pool attempt failed before pressure because measured daemon memory was 4,095,193,088 bytes, 199,774,208 bytes below the requested pool. `Job::capacity` refused admission before creating a meter volume or container; the journal was cleaned and settled with no objects. That attempt has no pressure credit.

A 3072 MiB run before the pre-finish observation handoff reached kernel ENOSPC but blocked when the test joined the job while `Run::finish` waited for archive SH behind the paused GC EX lock. The exact recorder was interrupted. Its two run generations were reconciled; the second journal was cleaned but not settled, its seven objects were removed, and the lease was released through the broker. The raw journal/database snapshots are withheld because they contain private grant-bearing state. This attempt has no pressure credit. The reviewed test-only handoff moved observation to after durable cleanup and before `Run::finish` requests archive SH.

The evidence ledger also records the retained pre-dispatch pin-mismatch, empty retry folder, incomplete M4 setup attempt, and ratchet-launch preflight failure. No result or status is inferred for the empty folder or any attempt without saved status metadata.

This profile does not cover APFS capture/hydration OOM, host or daemon-wide disk exhaustion, power loss, arbitrary storage backends, or a universal resource guarantee. Docker OOM coverage is limited to the bounded build cgroup in this fixture. No held-out evaluation, model call, or provider request was used.
