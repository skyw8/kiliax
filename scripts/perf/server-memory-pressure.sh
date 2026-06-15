#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${KILIAX_MEMORY_PORT:-18124}"
HOST="${KILIAX_MEMORY_HOST:-127.0.0.1}"
TOKEN="${KILIAX_MEMORY_TOKEN:-memory-test-token}"
BIN="${KILIAX_BIN:-}"
SESSION_COUNT="${SESSION_COUNT:-500}"
RUNS_PER_SESSION="${RUNS_PER_SESSION:-2}"
CONCURRENCY="${CONCURRENCY:-20}"
PAGE_PASSES="${PAGE_PASSES:-3}"
MESSAGE_SIZE="${MESSAGE_SIZE:-1024}"
MEMORY_SAMPLE_INTERVAL="${MEMORY_SAMPLE_INTERVAL:-1}"
MEMORY_MAX_RSS_KB="${MEMORY_MAX_RSS_KB:-}"
KEEP_MEMORY_LOG="${KEEP_MEMORY_LOG:-false}"
EVICTION_TEST="${EVICTION_TEST:-false}"
LIVE_SESSION_LIMIT="${LIVE_SESSION_LIMIT:-}"
RESULT=0

if [ -z "$LIVE_SESSION_LIMIT" ] && [ "$EVICTION_TEST" != "true" ]; then
  LIVE_SESSION_LIMIT=$((SESSION_COUNT + 16))
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi

if ! command -v awk >/dev/null 2>&1; then
  echo "awk is required" >&2
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
SAMPLER_PID=""
MEMORY_LOG="$TMP/memory.tsv"
PHASE_FILE="$TMP/phase"

cleanup() {
  if [ -n "$SAMPLER_PID" ] && kill -0 "$SAMPLER_PID" >/dev/null 2>&1; then
    kill "$SAMPLER_PID" >/dev/null 2>&1 || true
    wait "$SAMPLER_PID" >/dev/null 2>&1 || true
  fi
  if [ -n "$PID" ] && kill -0 "$PID" >/dev/null 2>&1; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi
  if [ "$KEEP_MEMORY_LOG" = "true" ]; then
    echo "memory log retained at: $MEMORY_LOG"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

set_phase() {
  printf '%s\n' "$1" > "$PHASE_FILE"
}

current_rss_kb() {
  local pid="$1"
  if [ -r "/proc/$pid/status" ]; then
    awk '/^VmRSS:/ { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/status"
    return
  fi
  ps -o rss= -p "$pid" 2>/dev/null | awk '{ print $1 + 0 }'
}

current_hwm_kb() {
  local pid="$1"
  if [ -r "/proc/$pid/status" ]; then
    awk '/^VmHWM:/ { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/status"
    return
  fi
  current_rss_kb "$pid"
}

sample_memory() {
  local pid="$1"
  printf 'time_s\tphase\trss_kb\thwm_kb\n' > "$MEMORY_LOG"
  while kill -0 "$pid" >/dev/null 2>&1; do
    record_memory_sample "$pid"
    sleep "$MEMORY_SAMPLE_INTERVAL"
  done
}

record_memory_sample() {
  local pid="$1"
  local phase rss hwm
  phase="$(cat "$PHASE_FILE" 2>/dev/null || printf 'unknown')"
  rss="$(current_rss_kb "$pid")"
  hwm="$(current_hwm_kb "$pid")"
  printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "$phase" "${rss:-0}" "${hwm:-0}" >> "$MEMORY_LOG"
}

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g' <<< "$1"
}

extract_session_id() {
  sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

post_session() {
  curl -fsS -X POST -H "$AUTH_HEADER" "$BASE/v1/sessions"
}

print_memory_summary() {
  echo
  echo "== memory summary =="
  awk -v max_rss="${MEMORY_MAX_RSS_KB:-0}" '
    NR == 1 { next }
    {
      rss = $3 + 0
      hwm = $4 + 0
      if (!baseline_set && $2 != "startup") {
        baseline = rss
        baseline_set = 1
      }
      final = rss
      if (rss > peak_rss) peak_rss = rss
      if (hwm > peak_hwm) peak_hwm = hwm
      samples += 1
    }
    END {
      if (!baseline_set) baseline = final
      delta = final - baseline
      printf "Samples:\t%d\n", samples
      printf "Baseline RSS:\t%d KB\n", baseline
      printf "Final RSS:\t%d KB\n", final
      printf "Peak RSS:\t%d KB\n", peak_rss
      printf "Peak HWM:\t%d KB\n", peak_hwm
      printf "RSS Delta:\t%+d KB\n", delta
      if (max_rss > 0) {
        printf "RSS Limit:\t%d KB\n", max_rss
        if (peak_rss > max_rss || peak_hwm > max_rss) {
          printf "Result:\tFAILED memory limit\n"
          exit 1
        }
      }
      printf "Result:\tOK\n"
    }
  ' "$MEMORY_LOG"
}

MAX_LIVE_SESSIONS_LINE=""
if [ -n "$LIVE_SESSION_LIMIT" ]; then
  MAX_LIVE_SESSIONS_LINE="  max_live_sessions: $LIVE_SESSION_LIMIT"
fi

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
$MAX_LIVE_SESSIONS_LINE
EOF

mkdir -p "$TMP/workspace"
set_phase "startup"
"$BIN" server run \
  --host "$HOST" \
  --port "$PORT" \
  --workspace-root "$TMP/workspace" \
  --config "$TMP/kiliax.yaml" \
  --token "$TOKEN" > "$TMP/server.log" 2>&1 &
PID="$!"
sample_memory "$PID" &
SAMPLER_PID="$!"

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
set_phase "ready"
record_memory_sample "$PID"

echo "memory pressure parameters:"
echo "  sessions: $SESSION_COUNT"
echo "  runs/session: $RUNS_PER_SESSION"
echo "  run concurrency: $CONCURRENCY"
echo "  message size: $MESSAGE_SIZE bytes"
echo "  message page passes: $PAGE_PASSES"
echo "  sample interval: ${MEMORY_SAMPLE_INTERVAL}s"
if [ -n "$LIVE_SESSION_LIMIT" ]; then
  echo "  max live sessions: $LIVE_SESSION_LIMIT"
else
  echo "  max live sessions: config default"
fi

SESSION_IDS="$TMP/session-ids.txt"
RUN_URLS="$TMP/run-urls.txt"
RUN_STATUS="$TMP/run-status.txt"
PAGE_URLS="$TMP/page-urls.txt"
PAGE_STATUS="$TMP/page-status.txt"
MESSAGE_TEXT="$TMP/message.txt"
RUN_BODY="$TMP/run.json"

awk -v n="$MESSAGE_SIZE" 'BEGIN { for (i = 0; i < n; i++) printf "x" }' > "$MESSAGE_TEXT"
escaped_text="$(json_escape "$(cat "$MESSAGE_TEXT")")"
printf '{"input":{"type":"text","text":"%s"},"auto_resume":true}\n' "$escaped_text" > "$RUN_BODY"

set_phase "create_sessions"
echo
echo "== create sessions =="
start_ns="$(date +%s%N)"
for i in $(seq 1 "$SESSION_COUNT"); do
  session_json="$(post_session)"
  session_id="$(printf '%s' "$session_json" | extract_session_id)"
  if [ -z "$session_id" ]; then
    echo "failed to extract session id from: $session_json" >&2
    exit 1
  fi
  echo "$session_id" >> "$SESSION_IDS"
  if [ $((i % 100)) -eq 0 ]; then
    echo "created $i sessions" >&2
  fi
done
end_ns="$(date +%s%N)"
awk -v start="$start_ns" -v end="$end_ns" -v total="$SESSION_COUNT" 'BEGIN {
  elapsed = (end - start) / 1000000000
  printf "Summary:\n"
  printf "  Total sessions:\t%d\n", total
  printf "  Total:\t%.4f sec\n", elapsed
  if (elapsed > 0) printf "  Sessions/sec:\t%.4f\n", total / elapsed
}'

set_phase "enqueue_runs"
echo
echo "== enqueue runs =="
while IFS= read -r session_id; do
  for _ in $(seq 1 "$RUNS_PER_SESSION"); do
    echo "$BASE/v1/sessions/$session_id/runs" >> "$RUN_URLS"
  done
done < "$SESSION_IDS"

export AUTH_HEADER RUN_BODY
start_ns="$(date +%s%N)"
set +e
xargs -r -n 1 -P "$CONCURRENCY" sh -c '
  curl -sS -o /dev/null -w "%{http_code}\n" \
    -X POST \
    -H "$AUTH_HEADER" \
    -H "Content-Type: application/json" \
    --data-binary "@$RUN_BODY" \
    "$1"
' _ < "$RUN_URLS" > "$RUN_STATUS"
xargs_status=$?
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
awk_status=$?
set -e
if [ "$xargs_status" -ne 0 ] || [ "$awk_status" -ne 0 ]; then
  RESULT=1
fi

set_phase "read_message_pages"
echo
echo "== read message pages =="
for _ in $(seq 1 "$PAGE_PASSES"); do
  while IFS= read -r session_id; do
    echo "$BASE/v1/sessions/$session_id/messages?limit=50" >> "$PAGE_URLS"
  done < "$SESSION_IDS"
done

export AUTH_HEADER
start_ns="$(date +%s%N)"
set +e
xargs -r -n 1 -P "$CONCURRENCY" sh -c '
  curl -sS -o /dev/null -w "%{http_code}\n" \
    -H "$AUTH_HEADER" \
    "$1"
' _ < "$PAGE_URLS" > "$PAGE_STATUS"
xargs_status=$?
end_ns="$(date +%s%N)"
awk -v start="$start_ns" -v end="$end_ns" '
  {
    count[$1] += 1
    total += 1
    if ($1 != 200) unexpected += 1
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
' "$PAGE_STATUS"
awk_status=$?
set -e
if [ "$xargs_status" -ne 0 ] || [ "$awk_status" -ne 0 ]; then
  RESULT=1
fi

set_phase "settle"
sleep "$MEMORY_SAMPLE_INTERVAL"

print_memory_summary || RESULT=1

echo
echo "memory pressure test completed"
exit "$RESULT"
