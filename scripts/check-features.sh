#!/bin/sh
# Check that the engine compiles, without warnings, in every feature
# combination the spec's dependency chain allows (the presets plus each
# feature with only what it needs), that the default build has no database
# code, and that no build links SQLite (removed in 0.8; PostgreSQL is the
# only database).
#
#   scripts/check-features.sh            # cargo check each combination
#   scripts/check-features.sh build      # cargo build instead
set -eu
cd "$(dirname "$0")/.."
CMD="${1:-check}"
# `content`, `auth`, `forms` and `media` switch on `db-postgres` (the
# database layer) themselves; `jobs`, `mail` and `payments` (auth + jobs)
# too.
for f in \
  "" \
  "db-postgres" \
  "content" \
  "publish" \
  "auth" \
  "auth,roles" \
  "admin" \
  "admin,workflow" \
  "admin,visual-editor" \
  "admin,workflow,visual-editor" \
  "forms" \
  "media" \
  "media,forms" \
  "search" \
  "search,auth" \
  "api" \
  "api,roles" \
  "admin,media,api,search" \
  "jobs" \
  "mail" \
  "forms,jobs,mail" \
  "auth,mail" \
  "auth,forms" \
  "oidc" \
  "oidc,roles" \
  "admin,oidc" \
  "app,oidc" \
  "admin,forms,media,jobs" \
  "s3" \
  "publish,s3" \
  "media,s3" \
  "content,media,s3" \
  "payments" \
  "payments,content" \
  "payments,admin" \
  "cms" \
  "cms,forms" \
  "app" \
  "app,payments"; do
  echo "== features: ${f:-<default>}"
  if [ -z "$f" ]; then
    RUSTFLAGS="-D warnings" cargo "$CMD" -q -p engine-cli
  else
    RUSTFLAGS="-D warnings" cargo "$CMD" -q -p engine-cli --features "$f"
  fi
done
if cargo tree -p engine-cli -e normal | grep -q -E "sqlx|engine-db|image|reqwest|engine-s3"; then
  echo "error: the default build depends on database, media or bucket code" >&2
  exit 1
fi
if cargo tree --workspace --all-features -e normal,build | grep -q -i "sqlite"; then
  echo "error: a build links SQLite; PostgreSQL is the only database (DECISIONS.md, v0.8)" >&2
  exit 1
fi
# With `visual-editor` off nothing of the editor is in the binary.
cargo build -q -p engine-cli --features "db-postgres,admin"
if grep -q -a "hammerwright-editor\|editor-dist\|@tiptap" target/debug/engine; then
  echo "error: an admin build without visual-editor contains the editor" >&2
  exit 1
fi
echo "Default build: no sqlx, no image, no HTTP client code. No SQLite anywhere. All combinations compile."
echo "An admin build without visual-editor contains none of the editor."
