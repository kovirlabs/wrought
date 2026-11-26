#!/bin/sh
# Run the whole test suite against SQLite: every database test gets its own
# database file in a temporary folder (no database container). Tests of the
# Postgres-only features (jobs, mail) are skipped; scripts/test-postgres.sh
# runs those. The S3 tests run too when the MinIO container
# (deploy/compose.minio.yml) is up; start it with
#   docker compose -f deploy/compose.minio.yml up -d --wait
#
#   scripts/test-sqlite.sh                 # all crates, all features
#   scripts/test-sqlite.sh -p engine-media
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
start_oidc
S3_ENV=""
if minio_up; then
  S3_ENV=$(minio_env)
else
  echo "note: MinIO is not running; the S3 tests are skipped" >&2
fi
# shellcheck disable=SC2086
env -u DATABASE_URL ENGINE_TEST_DATABASE_URL="sqlite:" $S3_ENV \
  cargo test --workspace --all-features "$@"
