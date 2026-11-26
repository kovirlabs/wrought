#!/bin/sh
# The query catalogue check (replaces the sqlx offline-data step of v0.2 to
# v0.4): prepare every statement the engine runs on its own tables against
# a fully migrated database of each backend. Takes seconds after the build.
#
#   scripts/check-queries.sh              # SQLite and PostgreSQL
#   scripts/check-queries.sh sqlite       # SQLite only (no container)
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
run() {
  ENGINE_TEST_DATABASE_URL="$1" cargo test -q -p engine-cli --all-features --test queries -- --nocapture
}
run "sqlite:"
if [ "${1:-}" != "sqlite" ]; then
  . scripts/lib.sh
  start_postgres
  run "postgres://wrought:wrought@127.0.0.1:${PORT}/wrought"
fi
