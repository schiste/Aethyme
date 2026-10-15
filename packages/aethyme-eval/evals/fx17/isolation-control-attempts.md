# FX17 unpaid isolation-control attempts

These records summarize four unpaid local isolation-control attempts. They do
not authorize or report a model trial. The complete argv, source snapshot,
profile, runtime profile, stdout, stderr, exit status, reports, and checksums
are retained in durable operator-controlled qualification storage. Raw macOS
kernel logs and diagnostic reports remain private there and are not committed.

The public frozen inputs remained unchanged in all four attempts:

- Payload SHA256: `e733e0183b5bc9d54ba65c3437109b170b9c348d13defd8fb7bcfc91c7bacd28`
- Schedule SHA256: `8d1474930397a2ce7c0bae1c6fccde6db5c32aba26ef5ad845945a91b150f065`

| Attempt | Source HEAD | Source tree | Runner SHA256 | Subject profile SHA256 | Result | Evidence SHA256 |
| --- | --- | --- | --- | --- | --- | --- |
| 001 | `301db343f947f8bb6cd3e860917585071273f5c8` | `a84cbe8f5639d833c64abf43c88aa542f9f17272` | `2ecb65464b736b02490e7bd7ec914ae1a19d9e92179b3edd639ef82484593005` | `41c1e8bbb4844501ac61a52f69a614319774eb2741e6ecdaeae796769b821681` | fail, CLI status 2 | `98db697da7cfdb76dd73abbe469a817cd32b81ada2bf6bd398f7ed02a8ba8bd3` |
| 002 | `9dfaecd4d9593a2ce339d69e76a1155d4fb7630a` | `981f56e86362230ffd7e842a647acb981c3bb985` | `a41457470c0aded5aee8ea91ca2e1ecd6bb076485de962322d59142d0c24a63a` | `5a0f572ee35e942b5ede27d4319825fe85e9d5a6dc251d48e77a330ce7e578f5` | fail, CLI status 2 | `9619dc731b193ea56d5a78439f02f73f33230b0be692376bc16ea827dd13ea26` |
| 003 | `dae783cfbf6837af69cf0ca8546fea0c98963ef9` | `deea88b2c517273ee4b1d36648a57df4992ccccc` | `4cfbc930140fcc00b6771287dd7dcd9053be871ba5bec5ae0c57ff3fe1c965a5` | `401b20db9bea3af2f0d3c096ad4532b37416790732805658ca168519d65c2aff` | fail, CLI status 2 | `e498cb252c1ba13636f8646df9c95ba01d48d363b605467af89ca332de6fe9f6` |
| 004 | `802f8f2ee0ace1620f36bba65f876d10e682cf1e` | `a84fe3c2fb9555a572ff79e1e88449124f424234` | `fb7cf8e21bea136356b0df578073547eae546b46dd56a52dc8a5b04c52a416e6` | `401b20db9bea3af2f0d3c096ad4532b37416790732805658ca168519d65c2aff` | fail, CLI status 2 | `4be5edffe5316153e9efb0ace02195a19b1c5ffc59e817482aba6f88215960c6` |

The raw report SHA256 values, in attempt order, are
`03be93bfc87710a0c3d94839d96576d987004468db33aa086fa5038085bb55b2`,
`428c0056fff97b1acc01db8076d76dd9145457353e2e932c88c6f1333582563e`, and
`359a70a21941803d20f2bf05d6f1e32dd5cb00cfdacf3b6ab9240b531b966c2c`, and
`4d85909598d67f4197c65ff2e7a64fd9d875570249f609e1a60b3dab0f6be385`. All four
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

No model call, export, or private held-out evaluation occurred. The precise
system-selector and shell-executable bootstrap paths were sent for independent
review before attempt 003. The profile retains its deny-default, network, fork,
and write denials, and continues to reject the foreign-executable canary.

## Attempt 003

The exact reviewed runner, profile, and control harness ran all eight subject
probes. The literal-argv candidate received the six exact arguments, produced
the expected stdout bytes, and exited 23. Its stderr also contained Bash's
startup diagnostic before the expected `stderr-sentinel`:

```text
shell-init: error retrieving current directory: getcwd: cannot access parent directories: Operation not permitted
```

The subject inherited the private `0700` temporary directory as its working
directory. The deny-default profile hid that directory's parent chain, so
Bash could not resolve its current directory. The report therefore failed its
exact stderr check. This was the sole report finding; the other seven subject
probes met their expected observations. The read and write canaries were
unchanged, the positive listener check succeeded while the candidate made no
connections, and fork and foreign-executable attempts were denied.

All five trusted-runner capture probes passed: stdout and stderr overflow
retained 65,536 bytes each before group termination; the watched response file
was observed at 65,537 bytes against a 65,536-byte threshold; timeout and
overflow process-tree probes killed and reaped the owned leaders and children.
The copied-approval retry was rejected, and the failed-CLI negative control
returned 2, persisted its report, and made zero trial-runner calls. The raw
report SHA256 is `359a70a21941803d20f2bf05d6f1e32dd5cb00cfdacf3b6ab9240b531b966c2c`;
its evidence SHA256 is `e498cb252c1ba13636f8646df9c95ba01d48d363b605467af89ca332de6fe9f6`.
The captured stdout SHA256 is `7d80e1386377f24d056f6a36b5981d80254859c4007ff5a5951665b01154fd46`;
stderr SHA256 is `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
Attempt 003 used runtime profile SHA256
`344525a875713778fae5344c053692db5b4aa21e8bd2a55929f18b2c5261940e` and runtime
correction SHA256 `96b0346c554d2c87513eedda8880a1ddfee870322b10fa943b6ca65bf5df7590`.
The complete source snapshot, invocation, full output streams, report,
summary, and checksum manifest remain in durable qualification storage. The
attempt-specific summary SHA256 is `5b32a792a47f194228d7cd4436af672b40cfcef6fd7e6e56081a7c68d752e557`.

## Attempt 004

The reviewed runner starts each sandboxed subject with `cwd=/`. All eight
subject probes ran. The literal-argv probe received the six exact arguments,
produced the exact expected stdout bytes, and exited 23; its stderr contained
Bash's `getcwd` diagnostic before the expected `stderr-sentinel`. This was the
only controls report failure; the other seven subject probes matched their
expected observations.

The narrow unified kernel log for the eight shell PIDs confirms the missing
operation: each Bash process was denied `file-read-metadata /` during startup.
The reviewed profile allowed file data for the exact root directory entry but
did not allow its metadata. The report raw SHA256 is
`4d85909598d67f4197c65ff2e7a64fd9d875570249f609e1a60b3dab0f6be385`; its
evidence SHA256 is
`4be5edffe5316153e9efb0ace02195a19b1c5ffc59e817482aba6f88215960c6`. The
CLI stdout SHA256 is
`20f5780744dd14e49a1771a667d7d8f4b500eebe8ba4366cd4ad8b9bdd8e6252`; CLI stderr
was empty, SHA256
`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.

All five trusted-runner capture probes passed, including both 64 KiB stream
overflows, the 65,537-byte response-file observation, and owned process-tree
termination and reaping. The failed-CLI negative control returned 2, persisted
its report, and made zero trial-runner calls. The consumed-campaign retry was
rejected without changing its marker. The exact unified-log extract and full
attempt evidence remain in durable private qualification storage. No model
call, private held-out evaluation, or FX17 export occurred. The private
attempt summary SHA256 is `049ef9bfe227256114631ac53dce6e3bd430e32338540d51e61ce53f60b30afa`;
the complete checksum-manifest SHA256 is
`80c82717ea56c86dea7c346c22788c434bd783ca9ad2cc1a467fcb083e9a63fb`.

## Runner correction after attempt 003

The follow-up runner starts sandboxed subject processes with `cwd=/`, using
the profile's existing root-directory-entry allowance. It adds no file-read,
metadata, executable, network, fork, or write allowances. Candidate code
remains in a private `0700` directory as a `0400` file, and the profile grants
read access only to that exact absolute file path. The subject profile SHA256
remains `401b20db9bea3af2f0d3c096ad4532b37416790732805658ca168519d65c2aff`.

The cwd runner correction received independent review before attempt 004. The
attempt failed only on the exact stderr check described above. The follow-up
profile change adds only `file-read-metadata` access for the exact literal `/` to
match the kernel denial; it is awaiting independent review before another
controls run. No model call or private held-out evaluation occurred.

- Runner SHA256 used by attempt 004: `fb7cf8e21bea136356b0df578073547eae546b46dd56a52dc8a5b04c52a416e6`
- Control harness SHA256: `56572eb5bbe886de9c610b2836eceefbac54eb4faccf49f2d738ef96a63c98a0`
- Attempt 004 subject profile SHA256: `401b20db9bea3af2f0d3c096ad4532b37416790732805658ca168519d65c2aff`
- Runtime profile SHA256: `344525a875713778fae5344c053692db5b4aa21e8bd2a55929f18b2c5261940e`
- Stale runtime-correction SHA256 recorded by the attempt 004 runner (it did not match the document): `050feab619b4b78503956dfa7a71e2b0550f936bd5048d9761d6e1d0509a5826`
- Actual runtime-correction document SHA256 at attempt 004: `a6d2cd8289ef646cf281f6ef02c6d9bec0d0c8cc08a211fc8c492ed275d43d89`

## Profile correction after attempt 004

The candidate profile now retains the exact root file-data allowance and adds
one exact metadata allowance for `/`, which the attempt 004 kernel log showed
Bash requires for `getcwd` after starting in `/`. This does not permit reads under
any root descendant. All other profile rules remain unchanged. The source
profile hash, runner hash, and runtime-correction document hash below describe
this pending-reviewed change; the runtime-profile hash is unchanged. No
controls attempt 005 has run.

- Runner SHA256 with the profile and verified runtime-correction pin: `fe92d4b47e47dabb9596d156c6fc1e9adeb4a44656fe53f44d103a4270791479`
- Control harness SHA256: `56572eb5bbe886de9c610b2836eceefbac54eb4faccf49f2d738ef96a63c98a0`
- Subject profile SHA256 with the correction: `cb3db0c407c33e7199908b2885ca037fc41c001c81eef30156ca907a75e985ee`
- Runtime profile SHA256 (unchanged): `344525a875713778fae5344c053692db5b4aa21e8bd2a55929f18b2c5261940e`
- Runtime correction SHA256 with the correction: `a6d2cd8289ef646cf281f6ef02c6d9bec0d0c8cc08a211fc8c492ed275d43d89`
