# FX17 sealed model payload and final-proposal evaluator

The model runner freezes public inputs only. It does not read or address the
private oracle. The final-proposal evaluator is a separate parent-side program
that loads an externally supplied frozen oracle and passes a candidate-only
child the exact literal argv for each case.

## Freeze and verify public payloads

The prepare command checks the public fixture manifest
`b3846f2fcca9bc8fb3bbcad26c0adedbc96fba723eddbbb174a9f3545ba11abc` and the
independent brief manifest
`eebfff2ad44662c1c91b9477af1acb5349668f4da0b67894827e9f4396649e59`. It
preserves the original four core rows and the separately identified two-row
misleading supplement. Prompts are exact UTF-8 strings with recorded hashes.

Prepare into a new, empty output directory:

```sh
python3 packages/aethyme-eval/evals/fx17/run_fx17.py prepare \
  --fixture-root packages/aethyme-eval/fx17 \
  --briefs-root packages/aethyme-eval/evals/fx17/briefs \
  --output-root /private/tmp/aethyme-fx17-payload-unique
```

On branches where the public FX17 fixture has not yet been integrated, pass the
externally supplied frozen fixture directory with `--fixture-root`. `prepare`
requires an explicit `--output-root` and refuses to overwrite an existing
directory. It makes no model, subject, or controls execution.

The committed payload set can be checked without regenerating it:

```sh
python3 packages/aethyme-eval/evals/fx17/run_fx17.py verify \
  --freeze-root packages/aethyme-eval/evals/fx17/frozen
```

`verify` checks the freeze, schedule, six row IDs, prompts, and their hashes. It
also enforces the frozen payload SHA256
`e733e0183b5bc9d54ba65c3437109b170b9c348d13defd8fb7bcfc91c7bacd28` and
combined-schedule SHA256
`8d1474930397a2ce7c0bae1c6fccde6db5c32aba26ef5ad845945a91b150f065`. It
does not run a model or candidate.

## Isolation controls and model trial

`runtime-contract-correction.md` records the pinned Codex CLI and the actual
runtime limits. The schedule's retained `max_tokens_per_attempt: 32768` is a
nominal output-token telemetry threshold, not an enforced token ceiling. The
runner compares only observed output tokens with that threshold; unavailable
usage remains unknown. Provider retry behavior also remains unknown. The
runtime correction and exact runtime-profile hash must appear in trial approval.

Before local isolation controls run, an independent reviewer must review and
bind the current runner, control harness, and subject-profile hashes. The
`controls` command requires those exact reviewed hashes, the reviewer identity,
and explicit acknowledgement. It runs actual macOS probes for literal argv,
read/write/self-write canaries, a local network listener and positive control,
fork and foreign-executable denial, and recursive-shell timeout. Separate
unpaid host-side stress probes drive synthetic stdout, stderr, and watched
response-file overflow through the trusted bounded-capture loop. Owned Python
leader/child process groups exercise both timeout and overflow termination;
the harness records PID/nonce identities before termination and waits a bounded
period to confirm both identities are gone. These synthetic processes run
outside the candidate sandbox and do not call a model or access the network.
The controls report records exact per-probe profile text and hashes, argv, exit
status, retained stdout/stderr bytes and hashes, canary state, capture limits,
watched-file threshold observations, and process outcomes. Keep that report
outside the repository. If findings fail, the command still persists the full
report and returns a nonzero exit status.

The controls report is derived from the actual probe runs: it includes bounded
captured stream bytes and hashes, exact exit and process-reap observations,
canary before/after hashes, and the local listener positive and negative
observations. Its evidence hash binds those results to the runner, harness,
and subject-profile hashes. After this report exists and a separate trial
approval matches all frozen and runtime hashes, the once-only runner can be
invoked with explicit hash acknowledgements. Trial output must be outside the
repository and in a new, empty directory.

The candidate profile keeps deny-default, process-fork, network, and
file-write denials. Each sandboxed subject starts with `/` as its working
directory. Attempt 003 showed that Bash's startup `getcwd` check could not
traverse the hidden ancestors of the inherited private candidate directory
and added a diagnostic to stderr. The root directory entry is already
allowed, so the runner now uses `/` without adding profile access. The
candidate script stays in a private `0700` directory as a `0400` file, and
only its exact absolute path is allowlisted. The profile's system bootstrap
reads remain limited to the root directory entry, the exact `/bin/sh` and
`/bin/bash` executables, metadata for the exact `/private/var/select/sh`
selector, the system library and `/usr/lib` trees, locale data, and the exact
macOS dyld cache directory at
`/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld`. On this host, the
system selector resolves `/bin/sh` to `/bin/bash`; both exact executables are
therefore permitted as the shell bootstrap chain. The candidate app file is
separately allowlisted per probe. The profile does not allow general
executable directories, general reads under `/private/var`, or general reads
under `/System/Volumes` or other Preboot paths.

The trial approval must name an existing absolute `consumption_registry_path`
outside the repository. The runner creates an exclusive marker in that fixed
directory keyed by the frozen campaign schedule and payload hashes before the
trial starts. Copying the approval file does not reset that marker, and a failed
attempt leaves the marker consumed. This is a cooperative local-host control:
an operator who can change the approval, registry directory, or its contents
can bypass it; it is not a PKI-backed attestation system. Keep the registry on
durable storage and do not remove campaign markers.

```sh
python3 packages/aethyme-eval/evals/fx17/run_fx17.py run \
  --freeze-root packages/aethyme-eval/evals/fx17/frozen \
  --approval /private/tmp/aethyme-fx17-approval.json \
  --controls /private/tmp/aethyme-fx17-controls.json \
  --output-root /private/tmp/aethyme-fx17-trial-unique \
  --ack-schedule-sha256 <combined-schedule-sha256> \
  --ack-payloads-sha256 <payloads-sha256>
```

The runner pins Codex CLI `0.162.1` by its absolute executable path and SHA256,
makes one CLI invocation per assigned row, and does not retry rows. The
180-second process-group deadline, 8 MiB stdout and 2 MiB stderr capture bounds,
and 4 MiB last-response observation limit are recorded in results. The response
file limit triggers process-group termination when an overage is observed; it
is not a filesystem quota. Output-token ceilings and provider retries are not
claimed as enforced controls. Invoiced cost, usage, provider request count,
retry count, and token fields remain unknown unless actually reported.

### First approved launcher preflight

The first approved six-row invocation exited with status 2 before consuming the
campaign marker or creating `results.json`. The gate returned the validated
registry as a `Path`, then the once-only claim helper rejected that value
because its strict validator accepts the approval's canonical string. The
preflight stopped before marker creation and before any model CLI child
launch. All six assigned rows remain unstarted; no row credit is recorded and
no private evaluator was run. The repo-safe preflight evidence SHA256 is
`b36f8bedbccb14847c11cd40fe7e07cfd28c6f770fa4710a27f2dbbd5bbcb59c`. Raw
logs and the approval record remain in private qualification storage.

## Final-proposal evaluation

The private oracle bundle must be supplied to the evaluator by the trusted
parent process. The candidate child receives only the candidate script and
literal argv for one case at a time; it does not receive the oracle directory.
The evaluator requires the matching passing controls report and writes its
comparison hashes, case IDs, and match outcomes outside the repository; it
does not include the expected oracle streams in that report.

```sh
python3 packages/aethyme-eval/evals/fx17/evaluate_final_proposal.py \
  --candidate /private/tmp/final-proposal/app.sh \
  --oracle-dir /private/tmp/aethyme-fx17-private-oracles \
  --oracle-kind SX01 \
  --controls /private/tmp/aethyme-fx17-controls.json \
  --output /private/tmp/aethyme-fx17-final-evaluation.json
```

The evaluator verifies the exact recovered case-vector, provenance, and
recovery-record hashes before candidate execution. Supply the private oracle
bundle from a trusted parent process; it is kept outside the repository and is
never staged or included in the model payloads.

## Artifacts

- `briefs/`: exact independently frozen public briefs.
- `frozen/`: public six-row payload, schedule, manifests, and freeze hashes.
- `model-trial-attempts.md`: shareable outcome ledger for the once-only six-row trial.
- The private oracle bundle is supplied externally to the trusted evaluator.
  The model payload generator does not read it.
- `run_fx17.py`: public-payload preparation, integrity verification, bounded
  runner, and candidate-only subject execution.
- `fx17_controls.py`: unpaid local isolation-control harness and evidence
  verifier.
- `evaluate_final_proposal.py`: trusted parent-side exact oracle comparison.
- `payload-schema.json`: result-record fields and typed outcomes.
