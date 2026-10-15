# FX17 one-time model trial attempts

## Frozen scope and execution pins

The user approved exactly the six frozen public payloads once each:

- Payload JSONL: 19,535 bytes, SHA256 e733e0183b5bc9d54ba65c3437109b170b9c348d13defd8fb7bcfc91c7bacd28.
- Combined schedule SHA256: 8d1474930397a2ce7c0bae1c6fccde6db5c32aba26ef5ad845945a91b150f065.
- Model: gpt-6-luna, medium reasoning, tools disabled.
- One attempt per row, zero automatic retries, 180-second process deadline.
- Provider retry behavior and charge cap remain unknown. The 32,768 output-token value is a nominal threshold; output-token enforcement is unverified.

The one-time run used source commit e6e3109a0ea3e01c2ca1d5bfdcd9b48c4a284416, tree df776fdfcc1c676253592f0910052ff6f35a4bb2, runner SHA256 6d90f6bdaf303d23243397881be54ad39e5c8a90a073b5b7988230bbf8d7fff8, subject-profile SHA256 cb3db0c407c33e7199908b2885ca037fc41c001c81eef30156ca907a75e985ee, control-harness SHA256 56572eb5bbe886de9c610b2836eceefbac54eb4faccf49f2d738ef96a63c98a0, runtime-profile SHA256 344525a875713778fae5344c053692db5b4aa21e8bd2a55929f18b2c5261940e, and runtime-correction SHA256 a6d2cd8289ef646cf281f6ef02c6d9bec0d0c8cc08a211fc8c492ed275d43d89. Codex CLI 0.162.1 was pinned to SHA256 74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0.

The run required the six unpaid isolation-control attempts recorded in [isolation-control-attempts.md](isolation-control-attempts.md): four failed attempts were preserved, followed by two passing attempts. The final controls report raw SHA256 is af097b963b76df0596b7b693b5ee0f4f1d84aafccaaa5df72ed97e2bda0db0dd; its canonical evidence SHA256 is 36eacb523fdb4aa54589a9b663e9a530a587c3e554f47d00bcd5030a717cdfef.

## Prior preflight failure

Attempt 001 is preserved as a separate preflight failure with repo-safe record SHA256 b36f8bedbccb14847c11cd40fe7e07cfd28c6f770fa4710a27f2dbbd5bbcb59c. It stopped before the campaign marker was created and before any model CLI child launched. It contributes no row attempt or response.

## Attempt 002 outcome

The runner exited with numeric status 0 after invoking the Codex CLI exactly once for each assigned row. All six CLI processes exited 1 and the runner recorded cli_or_provider_error; there were zero response files, zero candidate files, zero refusals, and no row retry.

Each event stream reported the same provider rejection: HTTP 400, invalid_request_error, code invalid_json_schema, parameter text.format.schema. The provider rejected codex_output_schema because its root uses oneOf; the root schema must instead be an object without top-level oneOf, anyOf, allOf, enum, or not. No model response was returned. Provider request count, usage, retry count, and invoiced cost remain unknown.

| Frozen row | Status | CLI exit | Event bytes | Event-stream SHA256 |
| --- | --- | ---: | ---: | --- |
| original-core:SX01-alias:correct | cli_or_provider_error | 1 | 1,083 | c48bc47ca04c78e2b99bf8245fc1c4c8834bc87c064d41d127fe2c877df41f35 |
| original-core:SX01-alias:absent | cli_or_provider_error | 1 | 1,083 | 6131d348834f63649d479d3f7d9f01f095772f19b3f3365f1816fc50e094fc54 |
| original-core:SX02-limit:correct | cli_or_provider_error | 1 | 1,083 | 6922ec235bfa7eaf90be22e287c6201e977fd14da48bdaa54154db5b215849c6 |
| original-core:SX02-limit:absent | cli_or_provider_error | 1 | 1,083 | 044d56c2572ceaf3b41d755f754256c27f350562a17aa6d1857fdb7703c14fdc |
| supplemental-misleading:SX01-alias:misleading | cli_or_provider_error | 1 | 1,083 | 41db838e3cf32c3246b17113bf460571fb283c07aa7c8a4d5537db39c42d393f |
| supplemental-misleading:SX02-limit:misleading | cli_or_provider_error | 1 | 1,083 | 78a7aea64a165106bd107396f54ee5e23a4d77263f5b8b5b13d5f5b8bce2d069 |

All six CLI stderr files were empty (SHA256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855). The runner's combined stdout was 357 bytes with SHA256 f7f2d2402226ebd3ab46dc08d7d3b6fd1ac2189beae18433b507a86ed3627042; combined stderr was empty. The results file is 23,661 bytes with SHA256 75f10e5f753971f1aa52332650ea73575672480cd93a7937acb9d66c406ae77b.

The fixed campaign registry now contains its consumed marker (SHA256 2ee7bd03cafed8d59490d1d35bccf1b9039269e3acda193dac79d79970b421a1). No row will be retried or the marker reset under this approval. The private held-out evaluator was not run because no model response or candidate file exists; no substitute was evaluated.

The full private attempt bundle remains in qualification storage. Its 24-artifact inventory is recorded in evidence-manifest.json with SHA256 d1abf3f8576e6c001ce32e79d06397430572375a6058632701a36c0d5d189ff2. Raw event streams, invocation/completion records, the consumed marker, and prior failed attempts are not included in this repository.
