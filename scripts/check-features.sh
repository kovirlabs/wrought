#!/bin/sh
# Check that the engine compiles, without warnings, in every feature
# combination the spec's dependency chain allows (the presets plus each
# feature with only what it needs), and that the default build has no
# database code. Used by CI and by scripts/sqlx-prepare.sh (which runs it
# with DATABASE_URL set so every combination's queries are recorded).
#
#   scripts/check-features.sh            # cargo check each combination
#   scripts/check-features.sh build      # cargo build instead
set -eu
cd "$(dirname "$0")/.."
CMD="${1:-check}"
# `content`, `auth` and `forms` need a database backend (`db-postgres`;
# without one engine-db stops the build with a message saying so).
for f in \
  "" \
  "db-postgres" \
  "db-postgres,content" \
  "db-postgres,publish" \
  "db-postgres,auth" \
  "db-postgres,auth,roles" \
  "db-postgres,admin" \
  "db-postgres,admin,workflow" \
  "db-postgres,forms" \
  "jobs" \
  "mail" \
  "db-postgres,forms,jobs,mail" \
  "db-postgres,auth,mail" \
  "db-postgres,auth,forms" \
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
if cargo tree -p engine-cli -e normal | grep -q sqlx; then
  echo "error: the default build depends on sqlx" >&2
  exit 1
fi
echo "Default build: no sqlx. All combinations compile."
