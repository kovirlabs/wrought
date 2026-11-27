#!/bin/sh
# Surviving a sleeping or restarting database (DECISIONS.md, "Survive a
# sleeping database"): runs crates/engine-cli/tests/outage.rs against a
# PostgreSQL container of its own, `wrought-test-pgsleep`, which the test
# restarts, stops, kills and starts. The shared `wrought-test-postgres` is
# never touched (other suites use it). The container is removed afterwards,
# also on failure.
#
#   scripts/check-db-outage.sh
#
# WROUGHT_PGSLEEP_PORT overrides the port (default 55499, localhost only).
set -eu
cd "$(dirname "$0")/.."
NAME=wrought-test-pgsleep
PORT="${WROUGHT_PGSLEEP_PORT:-55499}"

cleanup() { docker rm -f -v "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM
cleanup

# Data on an anonymous volume (kept across the test's restarts, removed
# with the container); fsync left on, as in production.
docker run -d --name "$NAME" \
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
  cargo test -p engine-cli --all-features --test outage -- --nocapture "$@"
