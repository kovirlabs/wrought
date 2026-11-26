#!/bin/sh
# Run the whole test suite against SQLite: every database test gets its own
# database file in a temporary folder (no container needed). Tests of the
# Postgres-only features (jobs, mail) are skipped; scripts/test-postgres.sh
# runs those.
#
#   scripts/test-sqlite.sh                 # all crates, all features
#   scripts/test-sqlite.sh -p engine-media
set -eu
cd "$(dirname "$0")/.."
env -u DATABASE_URL ENGINE_TEST_DATABASE_URL="sqlite:" \
  cargo test --workspace --all-features "$@"
