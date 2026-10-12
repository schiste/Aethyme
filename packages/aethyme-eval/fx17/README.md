# FX17 controlled source evolution

This directory freezes the public contract and the 21 unchanged `app.sh` states (`00` through `20`) for the ignored native-controller evolution exercise. The controller test uses each declared retained delta exactly once: `C01` changes state `00` to `01`, through `C20` changing state `19` to `20`.

## Frozen inputs

- Proposal manifest SHA-256: `b3846f2fcca9bc8fb3bbcad26c0adedbc96fba723eddbbb174a9f3545ba11abc`
- Public contract SHA-256: `81d22e805fe6fa48424b1c83fe3bf54d1c9d0e76e5bb14d9cc8c20e84f3be13a`
- Independent baseline oracle: [`oracle/baseline-cases.json`](oracle/baseline-cases.json), 65 cases, SHA-256 `bfbec841810ac9269b34c628969a4c8207809b70716753d979750ecedb605a06`
- Frozen source review SHA-256: `de7d56d9cc37f68787540e298bd49577c3fdf83b51544e3113aed4928eebb74e`
- Frozen oracle provenance SHA-256: `6c6205982af3a7fc20d3e62134e4b30d9ad5ee7b3bb8f424a005a3296083829f`

The controlled source files and oracle vectors are copied unchanged. The sealed task directories are not part of this harness and are never executed here.

## Verification transport

The verifier accepts at most 32 inventory cases. To retain every one of the 65 frozen vectors without changing verifier limits, [`proxy.sh`](proxy.sh) invokes the subject literally as `/bin/sh /artifact/app.sh <argv>` for each vector and emits the joined actual exit/stdout/stderr observations in a fixed hex transcript. The independent expected transcript is [`oracle/baseline65-proxy-transcript.txt`](oracle/baseline65-proxy-transcript.txt), SHA-256 `db545a7b7e8f18a9dbeb6931dc327f99a6e8d074d039d08d60dfc57ff208d8bb`. One outer inventory assertion checks that full transcript digest; the proxy's own exit is zero only if it emits all rows. The script has no expected output values and uses no `eval` or candidate-derived command text.

The proxy bytes are archived as a separate harness snapshot and embedded byte-for-byte in the fixed `/bin/sh -c` verifier command because the current verifier container mounts only `/artifact`, not `/harness`. The candidate build recipe copies only `/source/app.sh` to `/outputs/app.sh`; it does not run the candidate source on the host or include proxy code in the candidate artifact.

## Effects and limits

The verifier container uses the pinned cached Docker image, `--network none`, a read-only artifact mount, and a private writable `/tmp` tmpfs used by the proxy to capture streams. This does not prove complete filesystem-effect absence. The 21 controlled states have a frozen static source review for forbidden effects; the behavioral run separately enforces the complete 65-case contract. No model task is exported or run by this harness. Any future sealed model execution needs separate authorization and a deny-default child/effect-observation design.

The actual service trial is an ignored test in `packages/aethyme/rust/crates/aethyme-broker/src/local_controller/fx17_tests.rs`. It is authored but has not been executed in this work session. Its report preserves each service report, source snapshot and contribution lineage, accepted parent chain, budget reservation accounting, elapsed time, and the temp source/store roots. Budget allocations are conservative local execution accounting, not an invoice, external quota, or price estimate.

The separately prepared model payloads are owned by [`evals/fx17`](../evals/fx17/); no model export is authorized by this controlled-evolution harness.
