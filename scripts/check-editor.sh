#!/bin/sh
# Drive the visual editor in a real browser (headless Chromium through
# Playwright): builds the engine (cms preset, which includes
# `visual-editor`), copies the cms example into target/editor-work,
# prepares it on PostgreSQL with two editor accounts, serves it and runs
# scripts/a11y/editor-e2e.mjs: add a block, rich text, reorder (keyboard
# and the preview's toolbar), click-to-edit in the preview, a design token,
# save and publish, reload, a version conflict from a second session, and
# the published page. axe-core checks the editor with a block open.
#
#   scripts/check-editor.sh
#
# Uses the Playwright install of scripts/check-a11y.sh (target/a11y-tools;
# run that once first, or set WROUGHT_A11Y_TOOLS). Port 59420
# (WROUGHT_EDITOR_PORT). The site's database is a throwaway one on
# wrought-test-postgres (started if needed), or on the server WROUGHT_PG_URL
# names (scripts/lib.sh, `check_db`); it is dropped afterwards.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
TOOLS=${WROUGHT_A11Y_TOOLS:-$ROOT/target/a11y-tools}
WORK=$ROOT/target/editor-work
PORT=${WROUGHT_EDITOR_PORT:-59420}
. scripts/lib.sh
[ -f "$TOOLS/node_modules/playwright/package.json" ] ||
  { echo "Playwright is not installed in $TOOLS: run scripts/check-a11y.sh once" >&2; exit 2; }

echo "== building the engine (cms preset)"
cargo build -q -p engine-cli --features cms
E=$ROOT/target/debug/engine

rm -rf "$WORK"
mkdir -p "$WORK/site"
(cd examples/cms-site && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$WORK/site" && tar -xf -)
check_pg_ready
SERVER=""
cleanup() {
  stop_pid "$SERVER"
  drop_check_dbs
}
trap cleanup EXIT
trap 'exit 130' INT TERM
check_db editor
ENGINE_DATABASE_URL=$CHECK_DB_URL
export ENGINE_DATABASE_URL
"$E" migrate --site "$WORK/site" >/dev/null
for f in "$WORK"/site/media-samples/*.jpg; do
  "$E" media add --site "$WORK/site" "$f" --alt "Sample picture" >/dev/null
done
"$E" content seed --site "$WORK/site" >/dev/null
"$E" build --site "$WORK/site" >/dev/null
E2E_PW1="first editor password, long enough"
E2E_PW2="second editor password, long enough"
export E2E_PW1 E2E_PW2
printf '%s' "$E2E_PW1" | "$E" user create --site "$WORK/site" --email one@example.org --name One --password-stdin >/dev/null
printf '%s' "$E2E_PW2" | "$E" user create --site "$WORK/site" --email two@example.org --name Two --password-stdin >/dev/null
"$E" user set --site "$WORK/site" --email two@example.org --role admin >/dev/null

"$E" serve --site "$WORK/site" --port "$PORT" --no-workers >"$WORK/serve.log" 2>&1 &
SERVER=$!
i=0
until curl -fs -o /dev/null "http://127.0.0.1:$PORT/"; do
  i=$((i + 1)); [ "$i" -le 100 ] || { cat "$WORK/serve.log"; exit 1; }
  sleep 0.2
done
cp scripts/a11y/editor-e2e.mjs "$TOOLS/editor-e2e.mjs"
# The seeded article with a callout and a panel is item 1.
node "$TOOLS/editor-e2e.mjs" --base "http://127.0.0.1:$PORT" --type article --id 1 \
  --login one@example.org --password-env E2E_PW1 --login2 two@example.org --password2-env E2E_PW2
echo "== the visual editor works end to end"
