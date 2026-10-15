# Local model-adapter checks, 2026-10-15

At clean aggregate source `caef9cc94ca47b6f75f1d712e3e85359ccad55e1`, the two
model-free regression modules passed all six tests (exit 0). This separately
confirms the five protocol controls and the gate-to-registry handoff control.
The earlier author run of five controls had no durable transcript; this ledger
records the new independent invocation and retains its full output and numeric
status. Counts are executions and overlap with earlier checks.

Three deliberate changes in isolated disposable copies each failed the same
named test with numeric pytest exit **1**: permitting root schema composition,
counting an error item as a tool call, and allowing both candidate and refusal.
The complete logs show the intended assertions; copied source was restored from
a unique backup in `finally`, and the actual aggregate source stayed unchanged.
These are Python assertion failures, not Rust exit 101 checks.

[ledger.json](ledger.json) binds every raw artifact by size and SHA-256. Metadata
records the exact command, source, mutation and restore checks. Temporary paths
in the mutation records identify disposable test roots that have been removed.
No model request, private behavioral evaluator or isolation control was run.
These checks do not supply candidate outputs or retrospectively qualify the six
failed campaign rows. Controls006 still applies only to its original runner.
