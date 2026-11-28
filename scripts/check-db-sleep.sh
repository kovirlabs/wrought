#!/bin/sh
# A sleeping PostgreSQL (DECISIONS.md, "Sleeping Postgres watchdog"): runs
# crates/engine-cli/tests/dbsleep.rs against a PostgreSQL container of its
# own, `wrought-test-pgsleep`, which the engine puts to sleep and wakes
# through the shipped hook (deploy/wrought-db-hook, `docker:` target), the
# same one a host uses. Holds the wake (asleep to the first page rendered
# per request) to a budget. The shared `wrought-test-postgres` is never
# touched. The container is removed afterwards, also on failure.
#
#   scripts/check-db-sleep.sh
#
# WROUGHT_PGSLEEP_PORT overrides the port (default 55498, localhost only);
# WROUGHT_DBSLEEP_BUDGET_MS the wake budget (median of 5 wakes; default
# 3000 ms: the medians measured on the development VM were 1.23-1.47 s,
# mostly `docker start`, see DECISIONS.md; a slower runner can raise it).
set -eu
cd "$(dirname "$0")/.."
NAME=wrought-test-pgsleep
PORT="${WROUGHT_PGSLEEP_PORT:-55498}"

cleanup() { docker rm -f -v "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM
cleanup

# No restart policy: a stopped (sleeping) server stays stopped until the
# engine wakes it.
docker run -d --name "$NAME" --restart no \
  -e POSTGRES_USER=wrought -e POSTGRES_PASSWORD=wrought -e POSTGRES_DB=wrought \
  -p "127.0.0.1:${PORT}:5432" \
  -v /var/lib/postgresql/data \
  postgres:17 >/dev/null

i=0
ok=0
while [ "$ok" -lt 3 ]; do
  if docker exec -e PGPASSWORD=wrought "$NAME" \
      psql -h 127.0.0.1 -U wrought -d wrought -Atc 'SELECT 1' >/dev/null 2>&1; then
    ok=$((ok + 1))
  else
    ok=0
  fi
  i=$((i + 1))
  if [ "$i" -gt 240 ]; then
    echo "$NAME did not become ready" >&2
    docker logs --tail 30 "$NAME" >&2
    exit 1
  fi
  sleep 0.25
done

env -u DATABASE_URL \
  ENGINE_TEST_PGSLEEP_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought" \
  ENGINE_TEST_PGSLEEP_CONTAINER="$NAME" \
  ENGINE_TEST_DB_HOOK="$(pwd)/deploy/wrought-db-hook" \
  WROUGHT_DBSLEEP_BUDGET_MS="${WROUGHT_DBSLEEP_BUDGET_MS:-3000}" \
  cargo test -p engine-cli --all-features --test dbsleep -- --nocapture --test-threads=1 "$@"
