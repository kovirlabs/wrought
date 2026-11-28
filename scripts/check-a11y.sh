#!/bin/sh
# Accessibility check of everything Hammerwright renders: TagCheck (`engine
# check`, the Tag stack's checker) over the built HTML, and axe-core as a
# second opinion over the pages served by `engine serve`, in light and
# dark colour schemes (and at a phone width; WCAG 2.2 A and AA).
#
#   scripts/check-a11y.sh                # every site below, plus the admin
#   scripts/check-a11y.sh docs static-blog   # only these sites
#
# Sites: the example sites, the docs (with the component gallery) and the
# admin of the cms example, signed in. Database sites get a throwaway
# database each on wrought-test-postgres (started if needed), or on the
# server HAMMERWRIGHT_PG_URL names (scripts/lib.sh, `check_db`), all dropped at
# the end. To
# check another site (a starter theme), add a line to SITES:
#
#   <name> <folder> <kind>
#
# where kind is `static` (build and check), `db` (database: migrate,
# seed, build, check) or `db+admin` (also sign in and crawl /admin/, and
# /dashboard/ when the site has one). A folder `new:<preset>:<theme>` is
# made with `engine new --preset <preset> --theme <theme>` (the starter
# themes).
#
# Needs Node.js 20+, npx and network access the first time: Playwright,
# @axe-core/playwright (pinned below) and Playwright's headless Chromium are
# installed into target/a11y-tools (override with HAMMERWRIGHT_A11Y_TOOLS);
# TagCheck runs through `engine check` (npx @kovirlabs/tagcheck@0.1.3).
# Ports 59310.. on 127.0.0.1 (HAMMERWRIGHT_A11Y_PORT). Exit code 1 when either
# checker reports a problem. HAMMERWRIGHT_A11Y_INSTALL_ONLY=1 installs the tools
# and stops.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)

SITES="
docs docs static
static-blog examples/static-blog static
cms-site examples/cms-site db+admin
small-app examples/small-app db
theme-static-docs new:static:docs static
theme-static-business new:static:business static
theme-cms-docs new:cms:docs db
theme-cms-business new:cms:business db
theme-cms-dashboard new:cms:dashboard db+admin
theme-app-docs new:app:docs db
theme-app-business new:app:business db
theme-app-dashboard new:app:dashboard db
"
PLAYWRIGHT_VERSION=1.61.1
AXE_PLAYWRIGHT_VERSION=4.14.1

TOOLS=${HAMMERWRIGHT_A11Y_TOOLS:-$ROOT/target/a11y-tools}
WORK=$ROOT/target/a11y-work
PORT=${HAMMERWRIGHT_A11Y_PORT:-59310}
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
. scripts/lib.sh

echo "== building the engine (app preset)"
cargo build -q -p engine-cli --features app
E=$ROOT/target/debug/engine

if [ ! -f "$TOOLS/node_modules/@axe-core/playwright/package.json" ] ||
   ! grep -q "\"version\": \"$PLAYWRIGHT_VERSION\"" "$TOOLS/node_modules/playwright/package.json" 2>/dev/null; then
  echo "== installing playwright@$PLAYWRIGHT_VERSION and @axe-core/playwright@$AXE_PLAYWRIGHT_VERSION into $TOOLS"
  mkdir -p "$TOOLS"
  [ -f "$TOOLS/package.json" ] || echo '{"private": true}' > "$TOOLS/package.json"
  npm install --prefix "$TOOLS" --no-audit --no-fund --save-exact \
    "playwright@$PLAYWRIGHT_VERSION" "@axe-core/playwright@$AXE_PLAYWRIGHT_VERSION" >/dev/null
fi
(cd "$TOOLS" && npx --no-install playwright install --only-shell chromium >/dev/null)
cp scripts/a11y/axe-check.mjs "$TOOLS/axe-check.mjs"
# Only the tools (for scripts/check-editor.sh and check-caching.sh in CI).
if [ "${HAMMERWRIGHT_A11Y_INSTALL_ONLY:-}" = 1 ]; then
  echo "== Playwright and Chromium are installed in $TOOLS"
  exit 0
fi

rm -rf "$WORK"
mkdir -p "$WORK"
SERVER=""
cleanup() {
  stop_pid "$SERVER"
  drop_check_dbs
}
trap cleanup EXIT
trap 'exit 130' INT TERM
if echo "$SITES" | awk '{print $3}' | grep -q '^db'; then check_pg_ready; fi

wait_for() {
  i=0
  until curl -fs -o /dev/null "$1" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -gt 100 ]; then echo "server at $1 did not start" >&2; return 1; fi
    sleep 0.2
  done
}

fail() { echo "$1" >> "$WORK/failed.txt"; echo "!! $1"; }

check_site() {
  name=$1 src=$2 kind=$3
  echo "== $name ($src, $kind)"
  site=$WORK/$name
  case $src in
    new:*)
      preset=$(echo "$src" | cut -d: -f2)
      theme=$(echo "$src" | cut -d: -f3)
      "$E" new "$site" --preset "$preset" --theme "$theme" >/dev/null
      ;;
    *)
      mkdir -p "$site"
      # A copy, so nothing is written into the repository.
      (cd "$src" && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$site" && tar -xf -)
      ;;
  esac
  case $kind in
    db*)
      check_db "a11y_$name"
      ENGINE_DATABASE_URL=$CHECK_DB_URL
      export ENGINE_DATABASE_URL
      "$E" migrate --site "$site" >/dev/null
      if [ -d "$site/media-samples" ]; then
        for f in "$site"/media-samples/*.jpg; do
          "$E" media add --site "$site" "$f" --alt "Sample picture" >/dev/null
        done
      fi
      if [ -d "$site/seed" ]; then "$E" content seed --site "$site" >/dev/null; fi
      ;;
    *) unset ENGINE_DATABASE_URL ;;
  esac
  "$E" build --site "$site" >/dev/null
  out=$site/public
  "$E" check --site "$site" --dir "$out" || fail "$name: TagCheck"

  # Pages to give axe: every HTML file of the build.
  (cd "$out" && find . -name '*.html' ! -name '404.html' | sed -e 's|^\.||' -e 's|index\.html$||' | sort) > "$site/paths.txt"

  if [ "$kind" = static ]; then
    "$E" serve --site "$site" --dir "$out" --port "$PORT" >"$site/serve.log" 2>&1 &
  else
    # small-app and the app themes: their built files only. Their request
    # pages use the Accounts, Forms and App UI components, which the docs
    # gallery covers.
    if [ "$name" = small-app ] || [ "${src#new:app:}" != "$src" ]; then
      "$E" serve --site "$site" --dir "$out" --port "$PORT" >"$site/serve.log" 2>&1 &
    else
      "$E" serve --site "$site" --port "$PORT" --no-workers >"$site/serve.log" 2>&1 &
    fi
  fi
  SERVER=$!
  wait_for "http://127.0.0.1:$PORT/" || { cat "$site/serve.log"; fail "$name: serve"; return; }
  node "$TOOLS/axe-check.mjs" --base "http://127.0.0.1:$PORT" --paths "$site/paths.txt" --max 200 ||
    fail "$name: axe"

  if [ "$kind" = db+admin ]; then
    echo "== $name admin (signed in)"
    A11Y_PASSWORD="a11y check password, long enough"
    export A11Y_PASSWORD
    printf '%s' "$A11Y_PASSWORD" |
      "$E" user create --site "$site" --email a11y@example.org --name "A11y Check" --password-stdin >/dev/null
    printf '/admin/\n/account/login/\n/account/reset/\n/search/?q=iron\n' > "$site/admin-paths.txt"
    # The visual editor, with a block open (rich text, token choices).
    if grep -q 'type = "blocks"' "$site"/types/*.toml 2>/dev/null; then
      printf '/admin/editor/article/1/?block=1\n/admin/editor/article/1/?block=2\n' >> "$site/admin-paths.txt"
    fi
    if grep -q '^path = "/dashboard/"' "$site/site.toml"; then echo /dashboard/ >> "$site/admin-paths.txt"; fi
    node "$TOOLS/axe-check.mjs" --base "http://127.0.0.1:$PORT" --paths "$site/admin-paths.txt" \
      --login a11y@example.org --password-env A11Y_PASSWORD --crawl /admin/ --max 60 \
      --save "$site/admin-html" || fail "$name: axe (admin)"
    # The admin's pages are rendered per request: TagCheck their HTML too.
    "$E" check --dir "$site/admin-html" || fail "$name: TagCheck (admin)"
  fi
  kill "$SERVER" 2>/dev/null || true
  wait "$SERVER" 2>/dev/null || true
  SERVER=""
}

ONLY="$*"
echo "$SITES" > "$WORK/sites.txt"
CHECKED=0
while read -r name src kind; do
  [ -n "$name" ] || continue
  if [ -n "$ONLY" ] && ! echo " $ONLY " | grep -q " $name "; then continue; fi
  check_site "$name" "$src" "$kind"
  CHECKED=$((CHECKED + 1))
done < "$WORK/sites.txt"
# Every listed site was checked (a command that reads stdin inside the loop
# would end it early, silently).
if [ -z "$ONLY" ] && [ "$CHECKED" -ne "$(grep -c . "$WORK/sites.txt")" ]; then
  fail "only $CHECKED of $(grep -c . "$WORK/sites.txt") sites were checked"
fi

if [ -s "$WORK/failed.txt" ]; then
  echo "== accessibility problems found:"
  sed 's/^/   /' "$WORK/failed.txt"
  exit 1
fi
echo "== no accessibility problems found by TagCheck or axe-core"
