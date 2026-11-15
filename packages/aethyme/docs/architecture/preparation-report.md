# Preparation report

Last Updated: 2026-11-15

`aethyme broker submit prepare --session <id> --json` reports completed
preparation steps in `steps`. Each step's `duration_ms` is the elapsed wall
clock time in milliseconds for the supervised step, including its immediate
storage and environment checks. It does not include shared cache lease waiting
or cache rotation that happened before step execution. The field is additive;
consumers should tolerate additional fields. A failed preparation returns an
error instead of a completed report, so this field is available on successful
reports only.

When the preparation held the repository's shared cache, the report also lists
each operation it ran under the cache lease in `shared_cache_operations`
(additive, omitted when empty): `operation` (`waiting_for_cache`, `rotation_recovery`,
`budget_inventory`, `rotation_drain` or `preparation_step`), `detail` (the step
name), `elapsed_ms`, `items_done`, `bytes_observed` and, for the budget
inventory, `outcome` (`absent`, `within_budget`, `over_budget`,
`within_budget_background`, `over_budget_background`,
`inconclusive_time_limit` or `inconclusive_entry_limit`). An inconclusive
inventory keeps the shared store; it is not rotated. The command metric's
`preparation` object carries the same list.

`shared_cache_background_measurement` (additive, omitted otherwise) is set when
the budget decision came from a background measurement or when this prepare
queued one: `decided_by` (`outcome`, `bytes`, `entries`, `complete`,
`completed_at_ms`, `age_ms` of the measurement used; the inventory entry's
`outcome` then ends in `_background` and no walk ran under the lease) and
`queued` (the reason: `inconclusive_time_limit`, `inconclusive_entry_limit`, or
`refresh` when a decision was taken from a measurement in the last quarter of
its trust). An `over_budget_background` decision only comes from a result
confirmed by two consecutive walks more than 5% over the budget. The command
metric's `preparation` object carries the same object.
