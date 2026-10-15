# T45 pressure and GC interleaving

## Scope and status

This folder freezes the finite T45 pressure assertions for source commit
`0cc24dc3c18157084cc5af50a98792f3cd67ab81`, which received independent static
review. The bounded Local checks are authorized and remain **UNRUN** while host
headroom recovers. No Cargo, APFS, or Docker trial has run. No model calls or
held-out evaluations are part of this profile.

T45 requires ENOSPC/OOM during capture, hydration, and build while GC runs,
with no lost retained source, partial success, or false verification pass.
The schedule below makes each overlap observable at the real operation
boundary. It uses synchronous GC lock acquisition for APFS operations and a
paused real GC move for Docker build pressure; merely starting a worker is not
counted as overlap.

## Frozen operation matrix

| Operation | Pressure boundary | GC overlap witness | Required result |
| --- | --- | --- | --- |
| Source capture | On an owned 512 MiB APFS image, pass the normal capture reservation floor, start copying a new 32 MiB source blob, then apply owned-file pressure while leaving 16 MiB for the capture journal. Observe raw `ENOSPC` from blob copying before the capture failure state is written. | Call real `gc::apply` on a precomputed plan after capture holds archive SH, once before pressure and once in the raw copy-error callback. Both calls must return `ArchiveInUse`. | Capture fails with storage error; no receipt, retained result row, or digest object; after unlinking only the owned pressure inode, recovery is clean, a fresh real GC apply removes the named orphan, and retained A/B snapshots reconstruct byte-for-byte. |
| Hydration | Reconstruct the retained 32 MiB source snapshot into a fresh execution workspace on the same owned APFS image; apply pressure after workspace creation so reconstruction itself reports raw `ENOSPC`. | Call real `gc::apply` on a precomputed plan after materialization holds archive SH, once before pressure and again inside the raw reconstruction-error callback. Both calls must return `ArchiveInUse`. | Materialization returns no inputs and leaves no execution temp root; after exact pressure cleanup, fresh real GC removes its named orphan and retained A/B snapshots plus the execution snapshot remain byte-identical. |
| Build | Use the pinned Docker image, owned build cgroup and bounded build tmpfs. The ENOSPC case observes the kernel diagnostic from a bounded tmpfs write. The OOM case requires the build observation and diagnostic to identify OOM. | Start the exact builder and wait for its readiness marker. A same-store real GC plan must move the explicitly named orphan; `GcHooks.after_moves` acknowledges that path is absent while GC still holds archive EX, then pauses. Signal pressure only after this acknowledgement and keep GC paused through the error observation. | Neither pressure case yields an artifact or successful verification. Release the GC gate, require the same real apply to finish reclaiming the exact orphan, and reconstruct the pinned retained source snapshot with the same ID and bytes. The independent stable build remains live. |

## Limits

This finite profile exercises APFS ENOSPC for capture and hydration, plus
owned Docker build tmpfs ENOSPC and build-cgroup OOM. It does not claim
capture/hydration OOM coverage, host or daemon store exhaustion, power-loss
behavior, arbitrary storage backends, or a universal resource guarantee.
Failure paths retain their journals and owned roots for reconciliation; the
test removes only pressure files whose create-new descriptor identity matches
the pathname entry.

The frozen source assertions are in the ignored tests
`actual_native_t45_capture_and_hydration_enospc_interleave_with_gc` and
`actual_pressure_preserves_retained_source_and_bounded_native_cleanup`.
Runtime results must be appended as new source-bound attempt records and must
preserve every failure and unrun stage.
