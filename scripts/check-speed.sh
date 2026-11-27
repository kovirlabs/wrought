#!/bin/sh
# Speed guard: the `cms` preset serving examples/cms-site on PostgreSQL,
# release build, the engine pinned to one CPU and the load tool (wrk, the
# benchmark's `loadgen` image) to another, must answer at least a recorded
# number of requests a second for
#   file    a published article page served from disk by `engine serve`
#   render  the same article rendered per request (render dial `request`)
#   search  the search page, a different word on each request
# Fails when one falls below its floor, so a slowdown is a decision, not an
# accident. Floors are 62 % of the mean this script measured over five runs
# on the benchmark machine (file 7,160, render 1,220, search 2,530 a
# second; DECISIONS.md, "Speed guard"); raise them with a reason, lower them
# only with a measurement that explains why. A slower CI runner sets its
# own floors through the variables below.
#
#   scripts/check-speed.sh        # uses wrought-test-postgres (55432)
#
# Takes about 40 s after the build. Needs Docker (PostgreSQL and the load
# tool's container, wrought-bench-speed, removed after each load), taskset
# (util-linux) and at least two CPUs.
#   WROUGHT_SPEED_ENGINE_CPU / WROUGHT_SPEED_LOAD_CPU   CPUs to pin (1, 2)
#   WROUGHT_SPEED_FLOOR_FILE / _RENDER / _SEARCH          floors (req/s)
#   WROUGHT_SPEED_SECONDS                                 measured seconds (5)
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
FLOOR_FILE="${WROUGHT_SPEED_FLOOR_FILE:-4400}"
FLOOR_RENDER="${WROUGHT_SPEED_FLOOR_RENDER:-750}"
FLOOR_SEARCH="${WROUGHT_SPEED_FLOOR_SEARCH:-1550}"
SECONDS_MEASURED="${WROUGHT_SPEED_SECONDS:-5}"
. scripts/lib.sh

NCPU=$(nproc)
[ "$NCPU" -ge 2 ] || { echo "check-speed: needs at least two CPUs" >&2; exit 2; }
ECPU="${WROUGHT_SPEED_ENGINE_CPU:-$((1 % NCPU))}"
LCPU="${WROUGHT_SPEED_LOAD_CPU:-$((2 % NCPU))}"
[ "$ECPU" != "$LCPU" ] || LCPU=0

start_postgres
DB="wrought_speedcheck_$$"
psql_c -d wrought -c "CREATE DATABASE $DB" >/dev/null
WORK="$(mktemp -d)"
PID=""
stop_engine() {
  stop_pid "$PID"
  PID=""
}
cleanup() {
  stop_engine
  docker rm -f wrought-bench-speed >/dev/null 2>&1 || true
  psql_c -d wrought -c "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

cargo build --release -q -p engine-cli --features cms
docker image inspect wrought-bench-loadgen >/dev/null 2>&1 ||
  docker build -q -t wrought-bench-loadgen bench/loadgen >/dev/null
ENGINE="$PWD/target/release/engine"
cp -r examples/cms-site "$WORK/site"
rm -rf "$WORK/site/public" "$WORK/site/data"
export ENGINE_DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/$DB"
export ENGINE_SECRET_KEY="speed-check key, not a secret, 0123456789abcdef"
"$ENGINE" migrate --site "$WORK/site" >/dev/null
"$ENGINE" content seed --site "$WORK/site" >/dev/null
"$ENGINE" build --site "$WORK/site" >/dev/null
APORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
ARTICLE="/articles/flattening-a-plane-sole/"

start_engine() {
  taskset -c "$ECPU" "$ENGINE" serve --site "$WORK/site" --port "$APORT" >"$WORK/serve.log" 2>&1 &
  PID=$!
  i=0
  until curl -fs -o /dev/null "http://127.0.0.1:$APORT$ARTICLE"; do
    i=$((i + 1)); [ "$i" -gt 200 ] && { cat "$WORK/serve.log"; exit 1; }
    sleep 0.1
  done
}

# wrk URL [lua script]: requests a second over the measured seconds, after a
# two-second warm-up. Prints the figure; fails on any error response.
load() {
  set -- "$1" "${2:-}"
  extra=""
  [ -n "$2" ] && extra="-s /scripts/$2"
  run() {
    docker run --rm --name wrought-bench-speed --network host --cpuset-cpus "$LCPU" \
      -v "$PWD/bench/loadgen:/scripts:ro" wrought-bench-loadgen \
      -t 1 -c 32 --timeout 10s $extra -d "$1" "http://127.0.0.1:$APORT$2"
  }
  run 2s "$1" >/dev/null
  out=$(run "${SECONDS_MEASURED}s" "$1")
  if printf '%s\n' "$out" | grep -q "Non-2xx or 3xx responses"; then
    printf '%s\n' "$out" >&2
    echo "check-speed: error responses from $1" >&2
    exit 1
  fi
  printf '%s\n' "$out" | awk '/^Requests\/sec:/ {printf "%d\n", $2}'
}

FAILED=0
check() {
  name=$1 got=$2 floor=$3
  verdict=ok
  [ "$got" -ge "$floor" ] || { verdict="BELOW THE FLOOR"; FAILED=1; }
  printf '  %-8s %7s req/s   floor %6s   %s\n' "$name" "$got" "$floor" "$verdict"
}

# Published files and search, render dial as shipped (`publish`).
start_engine
FILE=$(load "$ARTICLE")
SEARCH=$(load "/search/?q=WORD" vary.lua)
stop_engine
# The same article rendered for each request.
sed -i 's/^default = "publish"$/default = "request"/' "$WORK/site/site.toml"
grep -q '^default = "request"$' "$WORK/site/site.toml"
start_engine
RENDER=$(load "$ARTICLE")
stop_engine

echo "engine serve (cms preset, PostgreSQL, release), engine on CPU $ECPU, wrk on CPU $LCPU, 32 connections, ${SECONDS_MEASURED} s each:"
check file "$FILE" "$FLOOR_FILE"
check render "$RENDER" "$FLOOR_RENDER"
check search "$SEARCH" "$FLOOR_SEARCH"
if [ "$FAILED" -ne 0 ]; then
  echo "FAIL: requests a second fell below a floor" >&2
  exit 1
fi
