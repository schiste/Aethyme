# FX17 sealed-model payload runner

The runner freezes only public model inputs. It does not read or address the private FX17 oracle.

The prepare command verifies the exact fixture manifest b3846f2fcca9bc8fb3bbcad26c0adedbc96fba723eddbbb174a9f3545ba11abc and independent brief manifest eebfff2ad44662c1c91b9477af1acb5349668f4da0b67894827e9f4396649e59, then writes reviewable exact prompts. It preserves the original fixture and its four core rows. The schedule reports the original four-row core and two separately frozen misleading rows, with six assigned rows total. Each row is once-final.

Prepare after the public fixture is integrated:

    python3 packages/aethyme-eval/evals/fx17/run_fx17.py prepare

Prepare from the reviewed frozen temporary inputs:

    python3 packages/aethyme-eval/evals/fx17/run_fx17.py prepare --fixture-root /private/tmp/aethyme-fx17-frozen-proposal-20261012 --briefs-root /private/tmp/aethyme-fx17-independent-briefs.20261012-53675-400qxb --output-root packages/aethyme-eval/evals/fx17/frozen

The prepare command makes no model, network, native, or evaluation call. It refuses payload overwrite and preserves exact brief files and hashes.

The run command requires all of: a separate explicit approval JSON matching the fixture, brief, payload, schedule, model, attempt and bound hashes; an independently reviewed macOS controls report; and command-line acknowledgement of the schedule and payload hashes. Controls must verify forbidden effects did not occur: canary read unchanged, no canary write, network server observed zero, private canary nonce absent in output, child process marker absent, and these remain true even if the subject exits successfully. The independent report also covers literal argv, captured output/status, model tool isolation, retries, and token bounds. The approval file is single-use and each run output directory is unique. No auto-retry is performed.

A trial uses gpt-6-luna with medium reasoning, a 180 second process-group timeout, zero provider retries configured, tools disabled, and a configured output-token limit of 32,768. Invoiced cost is stored as null unless separately observed. Usage fields and provider access/retry observations are retained; unavailable values remain null. The original four rows and two supplements are kept distinct in results with the full denominator of six. No FX17 model call has been authorized or run.

Candidate source is placed in one read-only file under a private temporary directory, and the exact file path is the only non-system read granted to the child. Stdin is closed; argv values are passed directly as literal argv. The helper requires a passing independent-controls report before invocation. The profile denies writes, networking and process forks; it allows /bin/sh and read-only system libraries. Since the profile permits /bin/sh as the interpreter executable, it can allow recursive shell exec in the same process; no foreign executable or fork is allowed, and the parent process-group deadline bounds execution. This helper never invokes a fixture oracle.

Files:
- briefs/: exact independently frozen public briefs.
- frozen/independent-brief-manifest.json: unchanged independent reviewer manifest.
- frozen/supplemental-manifest.json: separate two-row amendment.
- frozen/combined-schedule.json: four original core rows plus two supplemental rows.
- frozen/payloads.jsonl: exact six public prompts and provenance.
- frozen/freeze.json: payload and schedule hashes.
- payload-schema.json: result record fields.
