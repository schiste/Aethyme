# FX17 unpaid isolation-control attempts

These records summarize two unpaid local isolation-control attempts. They do
not authorize or report a model trial. The complete argv, source snapshot,
profile, runtime profile, stdout, stderr, exit status, reports, and checksums
are retained in durable operator-controlled qualification storage. Raw macOS
kernel logs and diagnostic reports remain private there and are not committed.

The public frozen inputs remained unchanged in both attempts:

- Payload SHA256: `e733e0183b5bc9d54ba65c3437109b170b9c348d13defd8fb7bcfc91c7bacd28`
- Schedule SHA256: `8d1474930397a2ce7c0bae1c6fccde6db5c32aba26ef5ad845945a91b150f065`

| Attempt | Source HEAD | Source tree | Runner SHA256 | Subject profile SHA256 | Result | Evidence SHA256 |
| --- | --- | --- | --- | --- | --- | --- |
| 001 | `301db343f947f8bb6cd3e860917585071273f5c8` | `a84cbe8f5639d833c64abf43c88aa542f9f17272` | `2ecb65464b736b02490e7bd7ec914ae1a19d9e92179b3edd639ef82484593005` | `41c1e8bbb4844501ac61a52f69a614319774eb2741e6ecdaeae796769b821681` | fail, CLI status 2 | `98db697da7cfdb76dd73abbe469a817cd32b81ada2bf6bd398f7ed02a8ba8bd3` |
| 002 | `9dfaecd4d9593a2ce339d69e76a1155d4fb7630a` | `981f56e86362230ffd7e842a647acb981c3bb985` | `a41457470c0aded5aee8ea91ca2e1ecd6bb076485de962322d59142d0c24a63a` | `5a0f572ee35e942b5ede27d4319825fe85e9d5a6dc251d48e77a330ce7e578f5` | fail, CLI status 2 | `9619dc731b193ea56d5a78439f02f73f33230b0be692376bc16ea827dd13ea26` |

The raw report SHA256 values, in attempt order, are
`03be93bfc87710a0c3d94839d96576d987004468db33aa086fa5038085bb55b2` and
`428c0056fff97b1acc01db8076d76dd9145457353e2e932c88c6f1333582563e`. Both
attempts used control harness SHA256
`56572eb5bbe886de9c610b2836eceefbac54eb4faccf49f2d738ef96a63c98a0` and
runtime profile SHA256
`344525a875713778fae5344c053692db5b4aa21e8bd2a55929f18b2c5261940e`.

## Attempt 001

The candidate shell processes aborted during dyld startup before the probe
scripts ran. The report failed because candidate output, canary, and timeout
observations were unavailable. The first profile revision then received three
exact read allowances for the root directory entry, `/bin/sh`, and the
physical dyld cache directory.

## Attempt 002

All eight candidate-shell probes exited 1 before their candidate source ran.
Each recorded the same stderr:

```text
Error opening /private/var/select/sh: Operation not permitted
Failed to exec /bin/bash as variant for /bin/sh (1: Operation not permitted).
```

The private kernel log records both exact blocking operations: metadata read
of `/private/var/select/sh` and process execution of `/bin/bash`. On this host,
`/private/var/select/sh` selects `/bin/bash` as the `/bin/sh` variant. The
attempt therefore remains **failed**. Read, write, network, fork, and foreign
executable results from candidate scripts are inconclusive when those scripts
did not start; their nonzero shell exit is not counted as evidence of denial.

The five unpaid trusted-runner capture probes passed: stdout and stderr were
retained at their 64 KiB caps before process-group termination; the watched
response file was observed at 65,537 bytes against a 65,536-byte threshold;
and timeout and overflow probes killed and reaped their owned leader and child.
The failed-controls CLI negative control returned status 2, persisted its full
report, and made zero trial-runner calls. The copied-approval negative control
was rejected by the consumed-campaign registry. These host-side controls do
not change the failed candidate-shell result.

No model call, export, or private held-out evaluation occurred. The remaining
system-selector and shell-executable bootstrap paths must be independently
reviewed before another controls attempt. The profile must retain its
deny-default, network, fork, and write denials, and continue to reject the
foreign-executable canary.

## Next reviewed profile

The follow-up profile adds metadata access for the exact shell selector and
read and execution access for exact `/bin/bash`, the shell variant selected by
`/bin/sh` on this host. It does not allow general executable directories or
general `/private/var` reads. This profile has not been executed; it requires
independent review before another controls attempt.

- Runner SHA256: `4cfbc930140fcc00b6771287dd7dcd9053be871ba5bec5ae0c57ff3fe1c965a5`
- Subject profile SHA256: `401b20db9bea3af2f0d3c096ad4532b37416790732805658ca168519d65c2aff`
- Runtime correction SHA256: `96b0346c554d2c87513eedda8880a1ddfee870322b10fa943b6ca65bf5df7590`
