#!/bin/sh
# Regenerate .sqlx/, the offline data for sqlx's compile-time checked
# queries, so `cargo build` works without a database. Run it after changing
# a `sqlx::query!` or the engine's own migrations, and commit .sqlx/.
#
# Uses a scratch database `wrought_sqlx` in the wrought-postgres container,
# built from the engine's migrations (not a site's).
set -eu
cd "$(dirname "$0")/.."
PORT="${WROUGHT_PG_PORT:-55432}"
docker compose -f deploy/compose.postgres.yml up -d --wait
psql_c() { docker exec -i wrought-postgres psql -q -v ON_ERROR_STOP=1 -U wrought "$@"; }
psql_c -d postgres -c "DROP DATABASE IF EXISTS wrought_sqlx WITH (FORCE)" -c "CREATE DATABASE wrought_sqlx"
psql_c -d wrought_sqlx -c "CREATE TABLE engine_migrations (id text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())"
for f in crates/engine-content/migrations/postgres/*.sql crates/engine-publish/migrations/postgres/*.sql; do
  psql_c -d wrought_sqlx < "$f"
done
rm -rf .sqlx && mkdir .sqlx
cargo clean -p engine-db -p engine-content -p engine-publish
DATABASE_URL="postgres://wrought:wrought@127.0.0.1:${PORT}/wrought_sqlx" SQLX_OFFLINE_DIR="$PWD/.sqlx" \
  cargo build --workspace --all-features --all-targets
echo "Wrote $(ls .sqlx | wc -l) query files to .sqlx/"
