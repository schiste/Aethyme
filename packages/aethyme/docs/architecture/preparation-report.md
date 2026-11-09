# Preparation report

`aethyme broker submit prepare --session <id> --json` reports completed
preparation steps in `steps`. Each step's `duration_ms` is the elapsed wall
clock time in milliseconds for the supervised step, including its immediate
storage and environment checks. It does not include shared cache lease waiting
or cache rotation that happened before step execution. The field is additive;
consumers should tolerate additional fields. A failed preparation returns an
error instead of a completed report, so this field is available on successful
reports only.
