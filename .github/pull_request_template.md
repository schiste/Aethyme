<!--
Aethyme PR template. Keep the sections; the `Contract:` line is read
by CI (see scripts/check-cross-process-contract.sh) to flag PRs that
touch externally-consumed entry points without an explicit decision.
-->

## Summary

<!-- 1–3 sentences on what changes and why. The "why" matters more
     than the "what" — diffs already show what. -->

## Contract

<!-- Pick exactly one. The values are not just labels; they trigger
     different review burdens. See
     `packages/aethyme/docs/architecture/cross-process-consumers.md`
     for what counts as a cross-process consumer. -->

- [ ] **none** — internal change. No effect on Python/Rust CLI entry
      points, deployed skill templates, hook scripts, or eval-pipeline
      output schemas.
- [ ] **introduce** — adds a new entry point, command, flag, output
      field, or schema that external consumers may begin to depend on.
      Required: document the new surface in
      `cross-process-consumers.md` (or, for output schemas, in the
      schema's own doc).
- [ ] **soft-retire** — marks an existing entry point as deprecated.
      Code is still callable; emits a deprecation notice. Required:
      keep the existing behavior working, plus a stderr deprecation
      warning, plus a note in `cross-process-consumers.md` with the
      retirement date.
- [ ] **hard-delete** — removes an existing entry point that's been
      soft-retired for at least one full eval cycle. Required: every
      consumer in `cross-process-consumers.md` must be updated in the
      same PR or already migrated; the consumer table row is also
      removed.

## Test plan

<!-- Concrete commands the reviewer can run. For UI/eval changes:
     what playground was run against and what was observed. -->

- [ ] `cd packages/aethyme && .venv/bin/python -m pytest tests/local/`
- [ ] (if Rust touched) `cd packages/aethyme/rust && cargo test --release -p aethyme-engine`
- [ ] (if eval pipeline touched) one full eval cycle on a Playground repo

## Eval impact

<!-- The cardinal rule: never modify tools/engine/pipeline/skills to
     improve eval scores. Confirm this PR survives the test:

       Would I make this exact change if the eval didn't exist?

     If the answer is no, do NOT merge. Add an entry to
     `packages/aethyme/docs/architecture/eval-tuning-rejected.md`
     explaining why it was proposed and rejected, and close the PR. -->

- [ ] This change would be made even if the eval didn't exist.
- [ ] No new task-type-specific code paths in engine, prompts, or
      scoring.
- [ ] No widening/tightening of an interface to match a known-failing
      eval scenario.
