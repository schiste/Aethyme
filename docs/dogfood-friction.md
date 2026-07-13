# Broker v0 Friction Log

Append-only. Each entry: date, type (blocker / noise / catch / gap),
what happened, cost or saving in minutes, action (issue filed / accepted / fixed).

| Date | Type | What happened | ± min | Action |
|------|------|---------------|-------|--------|
| 2026-07-13 | gap | Known going in: pip `aethyme` entrypoint shadows the Rust binary unless PATH is prefixed per shell. | -2/shell | #31 (already filed, priority:high) |
