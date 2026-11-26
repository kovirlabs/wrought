#!/bin/sh
# Run the whole test suite against SQLite: every database test gets its own
# database file in a temporary folder (no database container). Tests of the
# Postgres-only features (jobs, mail) are skipped; scripts/test-postgres.sh
# runs those. The single sign-on test uses the local Dex container
# (deploy/compose.oidc.yml, started here; WROUGHT_NO_OIDC=1 skips it).
#
#   scripts/test-sqlite.sh                 # all crates, all features
#   scripts/test-sqlite.sh -p engine-media
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
start_oidc
env -u DATABASE_URL ENGINE_TEST_DATABASE_URL="sqlite:" \
  cargo test --workspace --all-features "$@"
