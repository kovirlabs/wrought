#!/bin/sh
# Cold start of `engine serve` (DECISIONS.md, "Start on demand"): release
# build, cms preset, examples/cms-site on PostgreSQL (a database of its own
# on the shared wrought-test-postgres container, dropped afterwards).
# Measured by scripts/coldstart.py from just before the process is created
# to the first byte of the response:
#
#   1. socket activation (what systemd's wrought-site@.socket does), a
#      published page: the spec's target is under 200 ms;
#   2. the same with the database down (nothing listens at its address):
#      published pages are served without it;
#   3. a normal start (the engine binds its own port), a published page;
#   4. socket activation, the first page rendered per request, PostgreSQL
#      warm (the engine connects, checks its migrations and renders).
#
# Fails if the median of 1, 2 or 3 is over WROUGHT_COLDSTART_BUDGET_MS, or
# the median of 4 is over WROUGHT_COLDSTART_RENDER_BUDGET_MS, or any start
# answered other than 200 or did not exit cleanly on SIGTERM.
#
#   scripts/check-coldstart.sh
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
# Budgets (medians): the measurements of DECISIONS.md, "Start on demand"
# (published page about 8 ms, first page rendered per request about 95 ms
# on the development machine), with room for noise (a loaded machine gave medians up to 23 ms and 114 ms). The
# spec's target for a published page is 200 ms.
BUDGET_MS=${WROUGHT_COLDSTART_BUDGET_MS:-40}
RENDER_BUDGET_MS=${WROUGHT_COLDSTART_RENDER_BUDGET_MS:-200}
RUNS=${WROUGHT_COLDSTART_RUNS:-15}
PORT=${WROUGHT_PG_PORT:-55432}

start_postgres
DB="wrought_coldstart_$$"
psql_c -d wrought -c "CREATE DATABASE $DB" >/dev/null
WORK="$(mktemp -d)"
cleanup() {
  psql_c -d wrought -c "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

cargo build --release -q -p engine-cli --features cms
ENGINE="$PWD/target/release/engine"
cp -r examples/cms-site "$WORK/site"
rm -rf "$WORK/site/public" "$WORK/site/data"
UP="postgres://wrought:wrought@127.0.0.1:${PORT}/$DB"
export ENGINE_DATABASE_URL="$UP"
export ENGINE_SECRET_KEY="cold-start check key, not a secret, 0123456789abcdef"
"$ENGINE" migrate --site "$WORK/site" >/dev/null
"$ENGINE" content seed --site "$WORK/site" >/dev/null
"$ENGINE" build --site "$WORK/site" >/dev/null
# An address where nothing listens: the database is down.
DOWN_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
DOWN="postgres://wrought:wrought@127.0.0.1:${DOWN_PORT}/$DB"
export COLDSTART_LOG="$WORK/engine.log"

fail=0
# measure NAME BUDGET MODE PATH DATABASE_URL [ENGINE ARGS...]
measure() {
  name=$1 budget=$2 mode=$3 path=$4 url=$5
  shift 5
  echo "== $name"
  if ! out=$(ENGINE_DATABASE_URL="$url" python3 scripts/coldstart.py "$mode" "$RUNS" "$path" -- \
      "$ENGINE" serve --site "$WORK/site" "$@"); then
    echo "$out"
    echo "FAIL: $name: a start did not answer 200 or did not exit cleanly" >&2
    tail -20 "$COLDSTART_LOG" >&2
    fail=1
    return
  fi
  echo "$out" | tail -1
  median=$(echo "$out" | tail -1 | sed 's/median=\([0-9.]*\).*/\1/')
  if python3 -c "import sys; sys.exit(0 if float('$median') <= $budget else 1)"; then
    echo "ok: median ${median} ms (budget ${budget} ms)"
  else
    echo "$out"
    echo "FAIL: $name: median ${median} ms is over the budget of ${budget} ms" >&2
    fail=1
  fi
}

measure "published page, socket activation" "$BUDGET_MS" activate / "$UP"
measure "published page, socket activation, database down" "$BUDGET_MS" activate / "$DOWN"
measure "published page, normal start" "$BUDGET_MS" bind / "$UP" --port "{port}"
measure "first page rendered per request, socket activation, PostgreSQL warm" \
  "$RENDER_BUDGET_MS" activate "/search/?q=plane" "$UP"
exit "$fail"
