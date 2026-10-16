# Private independent FX08 behavior oracles

These files are review artifacts, not model inputs, model repairs or expected source implementations. Keep them outside every model-visible Playground. Each oracle expects only the trial's resulting `source.rs` beside it and uses `#[path = "source.rs"] mod subject`.

Coordinator validation command (not executed by this reviewer): `rustc --edition=2021 --test oracle.rs -o oracle-tests`, then run the bounded resulting executable. No Cargo or third-party dependency is needed. Preserve compilation failure as a real row failure; do not patch subject visibility/API to accommodate the tests. `requested_*` checks exercise follow-up requirements, while `survives_*` checks exercise earlier consumer obligations. A compilation failure prevents those categories from being evaluated separately and must be reported as such.

The six files contain ten requested tests and eight survivor tests. Counts describe test functions, not model trial denominators. The declared matrix remains 48 model trials, with eight once-only sealed trials after the declared stage.

Literal expectations were selected from written contracts/public tasks and recorded in ../oracle-derivation.md before reading baseline API declarations. The subsequent declaration extractor also printed baseline function bodies because the type declarations occupied one line; this exposure is recorded in manifest.json. Neither prior-base.rs nor any patched expected implementation was inspected, and no source implementation was created. The oracle vectors were not selected from composer/model results.

This correction version adds one literal creation-order permutation and strict cursor continuations to task 02; ../amendment.md records the independent derivation and why original calibration missed the creation key. Original oracle and manifest files remain unchanged in the original frozen directory.

Status: uncompiled/unrun independent review artifacts. The coordinator must independently validate compilation, intended positive controls and deliberate breaks before trial admission. No model, Rustc, Cargo, engine or browser trial was performed by this reviewer.
