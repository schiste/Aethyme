#!/bin/zsh
# run-eval.sh — Run an eval end-to-end: verify playground, start server, launch, wait, print results.
#
# Usage:
#   ./scripts/eval/run-eval.sh --eval-type dead-code --target mediawiki --model haiku
#   ./scripts/eval/run-eval.sh --eval-type bug-fix-1 --target mediawiki --model haiku --reasoning high

set -uo pipefail

# ── Parse arguments ──────────────────────────────────────────────────

EVAL_TYPE="" TARGET="" MODEL="haiku" REASONING="high" SKIP_VERIFY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --eval-type)    EVAL_TYPE="$2";  shift 2 ;;
        --target)       TARGET="$2";     shift 2 ;;
        --model)        MODEL="$2";      shift 2 ;;
        --reasoning)    REASONING="$2";  shift 2 ;;
        --skip-verify)  SKIP_VERIFY=true; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "$EVAL_TYPE" || -z "$TARGET" ]]; then
    echo "Usage: $0 --eval-type <type> --target <target> [--model <model>] [--reasoning <level>]"
    echo ""
    echo "Eval types: bug-fix, bug-fix-1, explain-repo, navigation-ctf, impact-analysis,"
    echo "            feature-localization, config-audit, dead-code, migration"
    echo "Targets:    aethyme, grc, mediawiki"
    echo "Models:     haiku (default), sonnet, opus"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AETHYME_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SERVER_DIR="$(cd "$AETHYME_ROOT/../aethyme-eval-ui/server" && pwd)"
SERVER_URL="http://localhost:8000"
SERVER_HEALTH_URL="$SERVER_URL/api/chau7/status"
SERVER_PYTHON="${SERVER_PYTHON:-$AETHYME_ROOT/.venv/bin/python}"
CURL_BIN="${CURL_BIN:-/usr/bin/curl}"

echo "=== Eval Run ==="
echo "  Type:      $EVAL_TYPE"
echo "  Target:    $TARGET"
echo "  Model:     $MODEL"
echo "  Reasoning: $REASONING"
echo ""

# ── Step 1: Verify playground ────────────────────────────────────────

if [[ "$SKIP_VERIFY" != true ]]; then
    echo ">>> Verifying playground..."
    if ! "$SCRIPT_DIR/verify-playground.sh" --target "$TARGET"; then
        echo ""
        echo "ERROR: Playground verification failed. Fix the issues above before running evals."
        echo "To skip verification: $0 ... --skip-verify"
        exit 1
    fi
    echo ""
fi

# ── Step 2: Ensure server is running ─────────────────────────────────

echo ">>> Checking eval server..."
if "$CURL_BIN" -sS -o /dev/null -w "" "$SERVER_HEALTH_URL" 2>/dev/null; then
    echo "  Server already running at $SERVER_URL"
else
    echo "  Starting server..."
    cd "$SERVER_DIR"
    "$SERVER_PYTHON" -m uvicorn main:app --port 8000 > /dev/null 2>&1 &
    SERVER_PID=$!
    SERVER_READY=false
    for _ in {1..20}; do
        sleep 1
        if "$CURL_BIN" -sS -o /dev/null "$SERVER_HEALTH_URL" 2>/dev/null; then
            SERVER_READY=true
            break
        fi
    done
    if [[ "$SERVER_READY" != true ]]; then
        echo "ERROR: Server failed to start"
        exit 1
    fi
    echo "  Server started (PID: $SERVER_PID)"
fi
echo ""

# ── Step 3: Prepare target snapshot ──────────────────────────────────

echo ">>> Preparing target snapshot..."
PREP_RESPONSE=$("$CURL_BIN" -sS -X POST "$SERVER_URL/api/repositories/prepare" \
    -H "Content-Type: application/json" \
    -d "{\"target\":\"$TARGET\"}")

PREP_READY=$(echo "$PREP_RESPONSE" | python3 -c "import sys,json; print('true' if json.load(sys.stdin).get('ready') else 'false')" 2>/dev/null)
PREP_ID=$(echo "$PREP_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null)

if [[ "$PREP_READY" != "true" ]]; then
    echo "ERROR: Target preparation is not ready"
    echo "$PREP_RESPONSE" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for err in data.get('errors', []):
    print(f'  {err}')
" 2>/dev/null
    exit 1
fi
echo "  Preparation ready: $PREP_ID"
echo ""

# ── Step 4: Launch eval ──────────────────────────────────────────────

echo ">>> Launching eval..."
RESPONSE=$("$CURL_BIN" -sS -X POST "$SERVER_URL/api/run" \
    -H "Content-Type: application/json" \
    -d "{\"evalType\":\"$EVAL_TYPE\",\"target\":\"$TARGET\",\"model\":\"$MODEL\",\"reasoning\":\"$REASONING\",\"preparationId\":\"$PREP_ID\"}")

STATUS=$(echo "$RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status','error'))" 2>/dev/null)
ERROR=$(echo "$RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('error','') or '')" 2>/dev/null)

if [[ "$STATUS" != "running" ]]; then
    echo "ERROR: Launch failed — $ERROR"
    exit 1
fi
echo "  Launched successfully"
echo ""

# ── Step 5: Poll for completion ──────────────────────────────────────

echo ">>> Waiting for completion..."
POLL_COUNT=0
MAX_POLLS=120  # 30 minutes at 15s intervals

while [[ $POLL_COUNT -lt $MAX_POLLS ]]; do
    sleep 15
    ((POLL_COUNT++))

    STATUS_PAIR=$("$CURL_BIN" -sS "$SERVER_URL/api/run/status" | python3 -c "
import json
import sys

try:
    data = json.load(sys.stdin)
except Exception:
    print('error\tstatus_parse_failed')
    raise SystemExit(0)

print(f\"{data.get('status', '?')}\t{data.get('currentPhase') or '?'}\")
" 2>/dev/null)
    RUN_STATUS="${STATUS_PAIR%%$'\t'*}"
    PHASE="${STATUS_PAIR#*$'\t'}"

    if [[ "$RUN_STATUS" == "complete" ]]; then
        echo "  Completed after $((POLL_COUNT * 15))s"
        break
    elif [[ "$RUN_STATUS" == "error" ]]; then
        echo "  ERROR during eval run (phase: $PHASE)"
        echo "$STATUS_JSON" | python3 -c "
import sys,json
d = json.load(sys.stdin)
for e in d.get('log', [])[-5:]:
    print(f'  {e}')
" 2>/dev/null
        exit 1
    fi

    # Progress indicator every minute
    if [[ $((POLL_COUNT % 4)) -eq 0 ]]; then
        echo "  ... $PHASE ($((POLL_COUNT * 15))s elapsed)"
    fi
done

if [[ $POLL_COUNT -ge $MAX_POLLS ]]; then
    echo "  TIMEOUT after 30 minutes"
    exit 1
fi

echo ""

# ── Step 6: Print results ────────────────────────────────────────────

echo ">>> Results:"
echo ""

"$CURL_BIN" -sS "$SERVER_URL/api/results?eval_type=$EVAL_TYPE&target=$TARGET" | python3 -c "
import sys, json

results = json.load(sys.stdin)
if not results:
    print('  No results found')
    sys.exit(0)

# Filter to most recent run_id
latest_run = max(set(r.get('runId','') for r in results))
results = [r for r in results if r.get('runId') == latest_run]

fmt = '{:<18} {:>7} {:>12} {:>8} {:>6} {:>6} {:>8} {:>5}'
print(fmt.format('Condition', 'Score', 'Tokens', 'Cost', 'Turns', 'Tools', 'Duration', 'CTO'))
print('-' * 80)
for r in sorted(results, key=lambda r: r.get('cost', 0)):
    tokens = r.get('totalTokens') or 0
    print(fmt.format(
        r.get('condition', '?'),
        f\"{r.get('score', 0):.1f}\",
        f'{tokens:,}',
        f\"\\\${r.get('cost', 0):.2f}\",
        str(r.get('turns', 0)),
        str(r.get('toolCalls', 0)),
        r.get('duration', '?'),
        r.get('cto', '?'),
    ))

print()
print(f'Run: {latest_run}')
print(f'View in UI: http://localhost:5173')
" 2>/dev/null

echo ""
echo "=== Done ==="
