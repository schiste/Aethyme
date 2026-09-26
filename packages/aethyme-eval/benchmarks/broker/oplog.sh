#!/bin/sh
# Append one event to an arm's operator log.
#   oplog.sh <arm-root> arm_start
#   oplog.sh <arm-root> arm_end
#   oplog.sh <arm-root> intervention <slot> <unstick|answer|resolve|restart|env|other> "<note>"
set -eu
[ $# -ge 2 ] || { echo "usage: $0 <arm-root> <arm_start|arm_end|intervention> [slot category note]" >&2; exit 2; }
exec python3 - "$@" <<'PY'
import datetime, json, sys
root, kind, rest = sys.argv[1], sys.argv[2], sys.argv[3:]
rec = {"ts": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"), "kind": kind}
if kind == "intervention":
    if len(rest) < 3:
        sys.exit("intervention needs <slot> <category> <note>")
    rec.update(slot=rest[0], category=rest[1], note=" ".join(rest[2:]))
elif kind not in ("arm_start", "arm_end"):
    sys.exit(f"unknown kind {kind!r}")
with open(f"{root}/operator_log.jsonl", "a") as fh:
    fh.write(json.dumps(rec) + "\n")
print(json.dumps(rec))
PY
