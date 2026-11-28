#!/bin/sh
# Idle memory budget: the `cms` preset serving examples/cms-site on
# PostgreSQL, after a warm-up of pages, search and the admin sign-in page,
# must stay under the recorded budget (resident set, KiB). Fails when it
# grows past it, so a memory regression is a decision, not an accident.
#
#   scripts/check-memory.sh            # uses wrought-test-postgres (55432)
#
# The budget was recorded with the release build on the benchmark machine
# (DECISIONS.md, "Engine memory budget"); raise it only with a reason.
set -eu
cd "$(dirname "$0")/.."
PORT="${HAMMERWRIGHT_PG_PORT:-55432}"
BUDGET_KIB="${HAMMERWRIGHT_MEMORY_BUDGET_KIB:-28672}"
. scripts/lib.sh

start_postgres
DB="hammerwright_memcheck_$$"
psql_c -d wrought -c "CREATE DATABASE $DB" >/dev/null
WORK="$(mktemp -d)"
cleanup() {
  stop_pid "${PID:-}"
  psql_c -d wrought -c "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

cargo build --release -q -p engine-cli --features cms
ENGINE="$PWD/target/release/engine"
cp -r examples/cms-site "$WORK/site"
rm -rf "$WORK/site/public" "$WORK/site/data"
export ENGINE_DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/$DB"
export ENGINE_SECRET_KEY="memory-check key, not a secret, 0123456789abcdef"
"$ENGINE" migrate --site "$WORK/site" >/dev/null
"$ENGINE" content seed --site "$WORK/site" >/dev/null
"$ENGINE" build --site "$WORK/site" >/dev/null
APORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
"$ENGINE" serve --site "$WORK/site" --port "$APORT" >"$WORK/serve.log" 2>&1 &
PID=$!
i=0
until curl -fs "http://127.0.0.1:$APORT/" >/dev/null; do
  i=$((i + 1)); [ "$i" -gt 100 ] && { cat "$WORK/serve.log"; exit 1; }
  sleep 0.1
done
for n in $(seq 1 20); do
  for p in / /articles/ /topics/ /search/?q=plane /search/?q=glass+$n /admin/login/; do
    curl -fs -o /dev/null "http://127.0.0.1:$APORT$p" || true
  done
done
sleep 3
RSS=$(awk '/^VmRSS:/ {print $2}' "/proc/$PID/status")
ANON=$(awk '/^RssAnon:/ {print $2}' "/proc/$PID/status")
echo "engine serve (cms preset, PostgreSQL) idle after warm-up: RSS ${RSS} KiB (anon ${ANON} KiB), budget ${BUDGET_KIB} KiB"
if [ "$RSS" -gt "$BUDGET_KIB" ]; then
  echo "FAIL: idle memory is over the budget" >&2
  exit 1
fi
