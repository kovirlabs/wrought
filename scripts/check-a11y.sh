#!/bin/sh
# Accessibility check of everything Wrought renders: TagCheck (`engine
# check`, the Tag stack's checker) over the built HTML, and axe-core as a
# second opinion over the pages served by `engine serve`, in light and
# dark colour schemes (and at a phone width; WCAG 2.2 A and AA).
#
#   scripts/check-a11y.sh                # every site below, plus the admin
#   scripts/check-a11y.sh docs static-blog   # only these sites
#
# Sites: the example sites, the docs (with the component gallery) and the
# admin of the cms example, signed in, on SQLite (no container needed). To
# check another site (a starter theme), add a line to SITES:
#
#   <name> <folder> <kind>
#
# where kind is `static` (build and check), `db` (SQLite database: migrate,
# seed, build, check) or `db+admin` (also sign in and crawl /admin/).
#
# Needs Node.js 20+, npx and network access the first time: Playwright,
# @axe-core/playwright (pinned below) and Playwright's headless Chromium are
# installed into target/a11y-tools (override with WROUGHT_A11Y_TOOLS);
# TagCheck runs through `engine check` (npx @kovirlabs/tagcheck@0.1.3).
# Ports 59310.. on 127.0.0.1 (WROUGHT_A11Y_PORT). Exit code 1 when either
# checker reports a problem.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)

SITES="
docs docs static
static-blog examples/static-blog static
cms-site examples/cms-site db+admin
small-app examples/small-app db
"
PLAYWRIGHT_VERSION=1.61.1
AXE_PLAYWRIGHT_VERSION=4.14.1

TOOLS=${WROUGHT_A11Y_TOOLS:-$ROOT/target/a11y-tools}
WORK=$ROOT/target/a11y-work
PORT=${WROUGHT_A11Y_PORT:-59310}
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}

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

rm -rf "$WORK"
mkdir -p "$WORK"
SERVER=""
cleanup() {
  if [ -n "$SERVER" ]; then kill "$SERVER" 2>/dev/null || true; fi
}
trap cleanup EXIT INT TERM

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
  mkdir -p "$site"
  # A copy, so nothing is written into the repository.
  (cd "$src" && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$site" && tar -xf -)
  case $kind in
    db*)
      export ENGINE_DATABASE_URL="sqlite://$site/site.db"
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
    # small-app asks for mail and jobs, which need PostgreSQL: serve its
    # built files only. Its request pages use the Accounts, Forms and App
    # UI components, which the docs gallery covers.
    if [ "$name" = small-app ]; then
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
while read -r name src kind; do
  [ -n "$name" ] || continue
  if [ -n "$ONLY" ] && ! echo " $ONLY " | grep -q " $name "; then continue; fi
  check_site "$name" "$src" "$kind"
done < "$WORK/sites.txt"

if [ -s "$WORK/failed.txt" ]; then
  echo "== accessibility problems found:"
  sed 's/^/   /' "$WORK/failed.txt"
  exit 1
fi
echo "== no accessibility problems found by TagCheck or axe-core"
