#!/bin/sh
# Run the whole test suite, including the tests that need PostgreSQL, an
# SMTP sink and an S3-compatible bucket, against the local
# `wrought-test-postgres`, `wrought-test-mailpit` and `wrought-test-minio`
# containers (deploy/compose.postgres.yml, deploy/compose.mailpit.yml,
# deploy/compose.minio.yml), and the single sign-on test against
# `wrought-test-oidc` (deploy/compose.oidc.yml; WROUGHT_NO_OIDC=1 skips it).
#
#   scripts/test-postgres.sh                 # all crates, all features
#   scripts/test-postgres.sh -p engine-publish
#
# Each database test creates and drops its own `wrought_test_<pid>_*`
# database, each bucket test its own `wrought-test-*` bucket. Leftovers of
# runs whose process is gone are dropped first; databases of a run that is
# still going are left alone, so two runs can overlap. Stop the containers
# afterwards with:
#   docker compose -f deploy/compose.postgres.yml down
#   docker compose -f deploy/compose.mailpit.yml down
#   docker compose -f deploy/compose.minio.yml down -v
#   docker compose -f deploy/compose.oidc.yml down -v
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
SMTP_PORT="${WROUGHT_SMTP_PORT:-51025}"
MAILPIT_PORT="${WROUGHT_MAILPIT_PORT:-58025}"
. scripts/lib.sh

start_postgres
compose_up deploy/compose.mailpit.yml
start_oidc
start_minio

drop_stale_test_databases

# DATABASE_URL is not needed (queries are checked by the catalogue test).
# shellcheck disable=SC2046
env -u DATABASE_URL \
  ENGINE_TEST_DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought" \
  ENGINE_TEST_SMTP_URL="smtp://127.0.0.1:${SMTP_PORT}" \
  ENGINE_TEST_MAILPIT_URL="http://127.0.0.1:${MAILPIT_PORT}" \
  $(minio_env) \
  cargo test --workspace --all-features "$@"
