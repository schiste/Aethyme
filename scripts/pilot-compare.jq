# Usage: jq -s -f scripts/pilot-compare.jq baseline.json followup.json
def fields: ["gate_runs", "gate_execution_ms", "gate_cache_hits",
  "estimated_gate_time_saved_ms", "conflicts_caught_pre_gate", "overlap_warnings",
  "command_calls", "command_execution_ms", "command_output_bytes",
  "command_output_sampled_calls"];
def counter:
  if type != "number" then error("counter must be numeric")
  elif . < 0 or floor != . or . > 9007199254740991 then error("invalid counter")
  else . end;
if length != 2 then error("provide exactly baseline and followup") else . end
| . as $reports
| if all(.[]; .schema_version == 1 and (.counters | keys) == (fields | sort))
  then . else error("incompatible pilot report schema") end
| {
    schema_version: 1,
    counter_deltas: (reduce fields[] as $field ({};
      (($reports[1].counters[$field] | counter) - ($reports[0].counters[$field] | counter)) as $delta
      | if $delta < 0 then error("counters reset/pruned: start a new baseline")
        else .[$field] = $delta end))
  }
