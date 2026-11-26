#!/bin/sh
# Check that the engine compiles, without warnings, in every feature
# combination the spec's dependency chain allows (the presets plus each
# feature with only what it needs, on each database backend), and that the
# default build has no database code.
#
#   scripts/check-features.sh            # cargo check each combination
#   scripts/check-features.sh build      # cargo build instead
set -eu
cd "$(dirname "$0")/.."
CMD="${1:-check}"
# `content`, `auth`, `forms` and `media` need a database backend
# (`db-postgres` or `db-sqlite`; without one engine-db stops the build with
# a message saying so). `jobs` and `mail` need `db-postgres`.
for f in \
  "" \
  "db-postgres" \
  "db-sqlite" \
  "db-postgres,db-sqlite" \
  "db-postgres,content" \
  "db-sqlite,content" \
  "db-postgres,publish" \
  "db-sqlite,publish" \
  "db-postgres,auth" \
  "db-sqlite,auth,roles" \
  "db-postgres,admin" \
  "db-sqlite,admin,workflow" \
  "db-postgres,forms" \
  "db-sqlite,forms" \
  "db-sqlite,media" \
  "db-postgres,media,forms" \
  "db-sqlite,search" \
  "db-postgres,search,auth" \
  "db-sqlite,api" \
  "db-postgres,api,roles" \
  "db-sqlite,admin,media,api,search" \
  "jobs" \
  "mail" \
  "db-postgres,forms,jobs,mail" \
  "db-postgres,auth,mail" \
  "db-postgres,auth,forms" \
  "db-postgres,oidc" \
  "db-sqlite,oidc,roles" \
  "db-sqlite,admin,oidc" \
  "app,oidc" \
  "db-sqlite,admin,forms,media,jobs" \
  "cms" \
  "cms,forms" \
  "app"; do
  echo "== features: ${f:-<default>}"
  if [ -z "$f" ]; then
    RUSTFLAGS="-D warnings" cargo "$CMD" -q -p engine-cli
  else
    RUSTFLAGS="-D warnings" cargo "$CMD" -q -p engine-cli --features "$f"
  fi
done
if cargo tree -p engine-cli -e normal | grep -q -E "sqlx|engine-db|image"; then
  echo "error: the default build depends on database or media code" >&2
  exit 1
fi
echo "Default build: no sqlx, no image code. All combinations compile."
