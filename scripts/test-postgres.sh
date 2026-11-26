#!/bin/sh
# Run the whole test suite, including the tests that need PostgreSQL and an
# SMTP sink, against the local `wrought-postgres` and `wrought-mailpit`
# containers (deploy/compose.postgres.yml, deploy/compose.mailpit.yml).
#
#   scripts/test-postgres.sh                 # all crates, all features
#   scripts/test-postgres.sh -p engine-publish
#
# Each database test creates and drops its own `wrought_test_<pid>_*`
# database. Leftovers of runs whose process is gone are dropped first;
# databases of a run that is still going are left alone, so two runs can
# overlap. Stop the containers afterwards with:
#   docker compose -f deploy/compose.postgres.yml down
#   docker compose -f deploy/compose.mailpit.yml down
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
SMTP_PORT="${WROUGHT_SMTP_PORT:-51025}"
MAILPIT_PORT="${WROUGHT_MAILPIT_PORT:-58025}"
. scripts/lib.sh

start_postgres
docker compose -f deploy/compose.mailpit.yml up -d --wait

drop_stale_test_databases

# DATABASE_URL is not needed (queries are checked by the catalogue test).
env -u DATABASE_URL \
  ENGINE_TEST_DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought" \
  ENGINE_TEST_SMTP_URL="smtp://127.0.0.1:${SMTP_PORT}" \
  ENGINE_TEST_MAILPIT_URL="http://127.0.0.1:${MAILPIT_PORT}" \
  cargo test --workspace --all-features "$@"
