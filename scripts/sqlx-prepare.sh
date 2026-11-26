#!/bin/sh
# Regenerate .sqlx/, the offline data for sqlx's compile-time checked
# queries, so `cargo build` works without a database. Run it after changing
# a `sqlx::query!` or the engine's own migrations, and commit .sqlx/.
#
#   scripts/sqlx-prepare.sh                # rebuild the schema, regenerate .sqlx/
#   scripts/sqlx-prepare.sh --schema-only  # only rebuild the scratch schema; then
#     DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought_sqlx cargo build ...
#
# Uses a scratch database `wrought_sqlx` in the wrought-postgres container,
# built from the engine's migrations (not a site's), in the order the
# engine applies them.
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
. scripts/lib.sh
start_postgres
psql_c -d postgres -c "DROP DATABASE IF EXISTS wrought_sqlx WITH (FORCE)" -c "CREATE DATABASE wrought_sqlx"
psql_c -d wrought_sqlx -c "CREATE TABLE engine_migrations (id text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())"
for f in crates/engine-content/migrations/postgres/*.sql \
         crates/engine-publish/migrations/postgres/*.sql \
         crates/engine-auth/migrations/postgres/0001_accounts.sql \
         crates/engine-auth/migrations/postgres/roles_0001_roles.sql \
         crates/engine-jobs/migrations/postgres/*.sql \
         crates/engine-forms/migrations/postgres/*.sql \
         crates/engine-admin/migrations/postgres/*.sql; do
  psql_c -d wrought_sqlx < "$f"
done
if [ "${1:-}" = "--schema-only" ]; then
  echo "Schema ready: postgres://wrought:wrought@127.0.0.1:${PORT}/wrought_sqlx"
  exit 0
fi
rm -rf .sqlx && mkdir .sqlx
cargo clean -p engine-db -p engine-content -p engine-publish -p engine-auth -p engine-admin \
  -p engine-jobs -p engine-mail -p engine-forms
export DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought_sqlx"
export SQLX_OFFLINE_DIR="$PWD/.sqlx"
cargo build --workspace --all-features --all-targets
# Some queries are only compiled without a feature (e.g. `roles` off):
# build every allowed combination so all of them are recorded.
scripts/check-features.sh build
echo "Wrote $(ls .sqlx | wc -l) query files to .sqlx/"
