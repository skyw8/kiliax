#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${KILIAX_LOAD_PORT:-18123}"
HOST="${KILIAX_LOAD_HOST:-127.0.0.1}"
TOKEN="${KILIAX_LOAD_TOKEN:-load-test-token}"
REQUESTS="${REQUESTS:-1000}"
CONCURRENCY="${CONCURRENCY:-20}"
BIN="${KILIAX_BIN:-}"

if ! command -v oha >/dev/null 2>&1; then
  echo "oha is required. Install with: cargo install oha --locked" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi

if [ -z "$BIN" ]; then
  cargo build -p kiliax
  if [ -x "$ROOT/target/debug/kiliax" ]; then
    BIN="$ROOT/target/debug/kiliax"
  else
    BIN="$(find "$ROOT/target" -path '*/debug/kiliax' -type f -perm -111 | sort | head -n 1)"
  fi
fi

if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  echo "failed to find executable kiliax binary; set KILIAX_BIN to override" >&2
  exit 1
fi

TMP="$(mktemp -d)"
PID=""
cleanup() {
  if [ -n "$PID" ] && kill -0 "$PID" >/dev/null 2>&1; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

cat > "$TMP/kiliax.yaml" <<EOF
default_model: test/test-model
providers:
  test:
    api: openai_chat_completions
    base_url: http://127.0.0.1:1
    models:
      - id: test-model
server:
  host: $HOST
  port: $PORT
  token: $TOKEN
EOF

"$BIN" server run \
  --host "$HOST" \
  --port "$PORT" \
  --workspace-root "$TMP/workspace" \
  --config "$TMP/kiliax.yaml" \
  --token "$TOKEN" > "$TMP/server.log" 2>&1 &
PID="$!"

BASE="http://$HOST:$PORT"
AUTH_HEADER="Authorization: Bearer $TOKEN"

for _ in $(seq 1 100); do
  if curl -fsS -H "$AUTH_HEADER" "$BASE/v1/capabilities" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$PID" >/dev/null 2>&1; then
    cat "$TMP/server.log" >&2
    exit 1
  fi
  sleep 0.1
done

curl -fsS -H "$AUTH_HEADER" "$BASE/v1/capabilities" >/dev/null

SESSION_JSON="$(curl -fsS -X POST -H "$AUTH_HEADER" "$BASE/v1/sessions")"
SESSION_ID="$(printf '%s' "$SESSION_JSON" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
if [ -z "$SESSION_ID" ]; then
  echo "failed to extract session id from: $SESSION_JSON" >&2
  exit 1
fi

RUN_BODY="$TMP/run.json"
cat > "$RUN_BODY" <<EOF
{"input":{"type":"text","text":"load test message"},"auto_resume":true}
EOF

run_oha() {
  local name="$1"
  shift
  echo
  echo "== $name =="
  env -u NO_COLOR oha --no-tui -n "$REQUESTS" -c "$CONCURRENCY" "$@"
}

run_oha "GET /v1/capabilities" -H "$AUTH_HEADER" "$BASE/v1/capabilities"
run_oha "POST /v1/sessions" -m POST -H "$AUTH_HEADER" "$BASE/v1/sessions"
run_oha "GET /v1/sessions" -H "$AUTH_HEADER" "$BASE/v1/sessions"
run_oha "GET /v1/sessions/{id}/messages" -H "$AUTH_HEADER" "$BASE/v1/sessions/$SESSION_ID/messages?limit=50"

RUN_URLS="$TMP/run-urls.txt"
RUN_STATUS="$TMP/run-status.txt"
echo "preparing $REQUESTS sessions for run creation load test..." >&2
for _ in $(seq 1 "$REQUESTS"); do
  run_session_json="$(curl -fsS -X POST -H "$AUTH_HEADER" "$BASE/v1/sessions")"
  run_session_id="$(printf '%s' "$run_session_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
  if [ -z "$run_session_id" ]; then
    echo "failed to extract run session id from: $run_session_json" >&2
    exit 1
  fi
  echo "$BASE/v1/sessions/$run_session_id/runs" >> "$RUN_URLS"
done

echo
echo "== POST /v1/sessions/{id}/runs =="
export AUTH_HEADER RUN_BODY
start_ns="$(date +%s%N)"
xargs -r -n 1 -P "$CONCURRENCY" sh -c '
  curl -sS -o /dev/null -w "%{http_code}\n" \
    -X POST \
    -H "$AUTH_HEADER" \
    -H "Content-Type: application/json" \
    --data-binary "@$RUN_BODY" \
    "$1"
' _ < "$RUN_URLS" > "$RUN_STATUS"
end_ns="$(date +%s%N)"
awk -v start="$start_ns" -v end="$end_ns" '
  {
    count[$1] += 1
    total += 1
    if ($1 != 201) unexpected += 1
  }
  END {
    elapsed = (end - start) / 1000000000
    printf "Summary:\n"
    printf "  Total requests:\t%d\n", total
    printf "  Total:\t%.4f sec\n", elapsed
    if (elapsed > 0) printf "  Requests/sec:\t%.4f\n", total / elapsed
    printf "\nStatus code distribution:\n"
    for (code in count) printf "  [%s] %d responses\n", code, count[code]
    exit unexpected ? 1 : 0
  }
' "$RUN_STATUS"

echo
echo "load test completed"
