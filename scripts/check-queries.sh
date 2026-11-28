#!/bin/sh
# The query catalogue check (replaces the sqlx offline-data step of v0.2 to
# v0.4): prepare every statement the engine runs on its own tables against
# a fully migrated PostgreSQL database. Takes seconds after the build.
#
#   scripts/check-queries.sh       # against wrought-test-postgres (55432)
#
# With HAMMERWRIGHT_PG_URL set (a running PostgreSQL, such as a CI service
# container) no container is started.
set -eu
cd "$(dirname "$0")/.."
PORT="${HAMMERWRIGHT_PG_PORT:-55432}"
if [ -n "${HAMMERWRIGHT_PG_URL:-}" ]; then
  URL="$HAMMERWRIGHT_PG_URL"
else
  . scripts/lib.sh
  start_postgres
  URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought"
fi
ENGINE_TEST_DATABASE_URL="$URL" cargo test -q -p engine-cli --all-features --test queries -- --nocapture
