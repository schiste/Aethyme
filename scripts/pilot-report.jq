# Offline allowlist: never copy gate/command names, repository identity, paths,
# task text, timestamps, or arbitrary fields into the exported report.
def counter:
  if type != "number" then error("counter must be numeric")
  elif . < 0 or floor != . or . > 9007199254740991 then error("invalid counter")
  else . end;
def total(field): map(.[field] | counter) | (add // 0) | counter;
if type != "object" or (.gates_executed | type) != "array" or (.commands | type) != "array"
then error("expected broker metrics JSON") else . end
| {
    schema_version: 1,
    counters: {
      gate_runs: (.gates_executed | total("runs")),
      gate_execution_ms: (.gates_executed | total("total_ms")),
      gate_cache_hits: (.gate_cache_hits | counter),
      estimated_gate_time_saved_ms: (.gate_time_saved_ms | counter),
      conflicts_caught_pre_gate: (.conflicts_caught_pre_gate | counter),
      overlap_warnings: (.overlaps_warned | counter),
      command_calls: (.commands | total("count")),
      command_execution_ms: (.commands | total("total_ms")),
      command_output_bytes: (.commands | total("total_output_bytes")),
      command_output_sampled_calls: (.commands | total("output_sampled_calls"))
    }
  }
