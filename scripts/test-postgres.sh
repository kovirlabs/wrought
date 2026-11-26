#!/bin/sh
# Run the whole test suite, including the tests that need PostgreSQL, against
# the local `wrought-postgres` container (deploy/compose.postgres.yml).
#
#   scripts/test-postgres.sh                 # all crates, all features
#   scripts/test-postgres.sh -p engine-publish
#
# Each database test creates and drops its own `wrought_test_*` database;
# leftovers from interrupted runs are dropped first. Stop the container
# afterwards with: docker compose -f deploy/compose.postgres.yml down
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
docker compose -f deploy/compose.postgres.yml up -d --wait
for db in $(docker exec wrought-postgres psql -U wrought -d postgres -Atc \
    "SELECT datname FROM pg_database WHERE datname LIKE 'wrought\_test\_%'"); do
  docker exec wrought-postgres psql -q -U wrought -d postgres -c "DROP DATABASE \"$db\" WITH (FORCE)"
done
# Unset DATABASE_URL so sqlx uses the committed offline query data (.sqlx/).
env -u DATABASE_URL \
  ENGINE_TEST_DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought" \
  cargo test --workspace --all-features "$@"
