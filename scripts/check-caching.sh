#!/bin/sh
# Browser caching end to end: a page refresh is all it takes to see an
# edit, and nothing more is fetched than needed (DECISIONS.md, "Browser
# caching"). Builds the engine (cms preset), copies the cms example into
# target/caching-work on PostgreSQL, adds a stylesheet linked through `asset()`
# to the article pages, serves it, and runs scripts/a11y/caching-e2e.mjs in
# headless Chromium twice: straight against `engine serve`, then through
# the shipped deploy/Caddyfile.publish (Caddy in a `wrought-test-caching`
# container on the host network, removed afterwards). Each run edits an
# article and the stylesheet, and publishes a title the search page shows.
# Header checks with curl on both paths: hashed files immutable, pages
# no-cache with an ETag per encoding, 304s, admin and account never stored.
#
#   scripts/check-caching.sh
#
# Uses the Playwright install of scripts/check-a11y.sh (target/a11y-tools;
# run that once first, or set HAMMERWRIGHT_A11Y_TOOLS). Ports 59430 (engine,
# HAMMERWRIGHT_CACHING_PORT) and 59431 (Caddy, HAMMERWRIGHT_CACHING_CADDY_PORT).
# HAMMERWRIGHT_CACHING_NO_CADDY=1 skips the Caddy run. The site's database is a
# throwaway one on wrought-test-postgres (started if needed), or on the
# server HAMMERWRIGHT_PG_URL names (scripts/lib.sh, `check_db`).
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
TOOLS=${HAMMERWRIGHT_A11Y_TOOLS:-$ROOT/target/a11y-tools}
WORK=$ROOT/target/caching-work
PORT=${HAMMERWRIGHT_CACHING_PORT:-59430}
CPORT=${HAMMERWRIGHT_CACHING_CADDY_PORT:-59431}
CADDY_IMAGE=caddy:2.11.8-alpine
CADDY=wrought-test-caching
. scripts/lib.sh
[ -f "$TOOLS/node_modules/playwright/package.json" ] ||
  { echo "Playwright is not installed in $TOOLS: run scripts/check-a11y.sh once" >&2; exit 2; }

# An engine left by an earlier run would hold the port (and could open the
# new site's database by its path): say so instead of failing obscurely.
require_free_port "$PORT"

echo "== building the engine (cms preset)"
cargo build -q -p engine-cli --features cms
E=$ROOT/target/debug/engine
S=$WORK/site

rm -rf "$WORK"
mkdir -p "$S"
(cd examples/cms-site && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$S" && tar -xf -)
# Article pages link a stylesheet of static/ through asset().
mkdir -p "$S/static/css"
printf 'h1 { color: rgb(1, 2, 3); }\n' >"$S/static/css/check.css"
cat >"$WORK/head.html" <<'EOF'
{% block head %}<link rel="stylesheet" href="{{ asset('/css/check.css') }}">{% endblock %}
EOF
sed -i '/{% extends "base.html" %}/r '"$WORK/head.html" "$S/templates/article.html"

check_pg_ready
SERVER=""
cleanup() {
  stop_pid "$SERVER"
  docker rm -f "$CADDY" >/dev/null 2>&1 || true
  drop_check_dbs
}
trap cleanup EXIT
trap 'exit 130' INT TERM
check_db caching
ENGINE_DATABASE_URL=$CHECK_DB_URL
export ENGINE_DATABASE_URL
"$E" migrate --site "$S" >/dev/null
"$E" content seed --site "$S" >/dev/null
"$E" build --site "$S" >/dev/null
SLUG=$("$E" content show --site "$S" article 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).slug))')
TITLE=$("$E" content show --site "$S" article 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const j=JSON.parse(s);console.log(j.title ?? j.fields.title)})')
ART=/articles/$SLUG/

"$E" serve --site "$S" --port "$PORT" --no-workers >"$WORK/serve.log" 2>&1 &
SERVER=$!
i=0
until curl -fs -o /dev/null "http://127.0.0.1:$PORT/"; do
  i=$((i + 1)); [ "$i" -le 100 ] || { cat "$WORK/serve.log"; exit 1; }
  sleep 0.2
done

fail() { echo "not ok - $*" >&2; tail -20 "$WORK/serve.log" >&2; exit 1; }
# The value of response header $2 (lower-case name) for curl arguments $3...
hdr() {
  name=$1; shift
  curl -fsS -o /dev/null -D - "$@" | tr -d '\r' | awk -v n="$name" '!f && tolower($0) ~ "^"n": " { sub(/^[^:]*: /, ""); print; f = 1 }'
}
status() { curl -sS -o /dev/null -w '%{http_code}' "$@"; }
# Wait until the page at $1 contains $2 (and, with $3, no longer $3).
wait_for() {
  i=0
  while :; do
    body=$(curl -fs "$1" || true)
    if printf '%s' "$body" | grep -qF -- "$2" &&
      { [ -z "${3:-}" ] || ! printf '%s' "$body" | grep -qF -- "$3"; }; then
      return 0
    fi
    i=$((i + 1)); [ "$i" -le 150 ] || fail "$1 never showed \"$2\""
    sleep 0.2
  done
}

# Header rules, on one serving path ($1: base URL, $2: name).
headers() {
  B=$1
  page=$(curl -fs "$B$ART")
  css=$(printf '%s' "$page" | grep -o '/_engine/tagbrush-[^"]*\.css' | head -1)
  mine=$(printf '%s' "$page" | grep -o '/css/check\.[0-9a-f]*\.css' | head -1)
  [ -n "$css" ] && [ -n "$mine" ] || fail "$2: the article links no hashed stylesheets"
  for a in "$css" "$mine"; do
    cc=$(hdr cache-control "$B$a")
    [ "$cc" = "public, max-age=31536000, immutable" ] || fail "$2: $a is \"$cc\""
  done
  cc=$(hdr cache-control "$B/css/check.css")
  [ "$cc" = "no-cache" ] || fail "$2: a plain-path file is \"$cc\""
  cc=$(hdr cache-control "$B$ART")
  [ "$cc" = "no-cache" ] || fail "$2: the article is \"$cc\""
  br=$(hdr etag -H 'Accept-Encoding: br' "$B$ART")
  gz=$(hdr etag -H 'Accept-Encoding: gzip' "$B$ART")
  id=$(hdr etag -H 'Accept-Encoding: identity' "$B$ART")
  [ -n "$br" ] && [ "$br" != "$gz" ] && [ "$gz" != "$id" ] || fail "$2: ETags per encoding: $br $gz $id"
  vary=$(hdr vary -H 'Accept-Encoding: br' "$B$ART")
  printf '%s' "$vary" | grep -qi accept-encoding || fail "$2: Vary is \"$vary\""
  [ "$(status -H 'Accept-Encoding: br' -H "If-None-Match: $br" "$B$ART")" = 304 ] || fail "$2: no 304 for the article"
  q="$B/search/?q=plane"
  et=$(hdr etag "$q")
  [ -n "$et" ] || fail "$2: the search page has no ETag"
  [ "$(hdr cache-control "$q")" = "no-cache" ] || fail "$2: the search page is not no-cache"
  [ "$(status -H "If-None-Match: $et" "$q")" = 304 ] || fail "$2: no 304 for the search page"
  for p in /account/login/ /admin/; do
    cc=$(curl -sS -o /dev/null -D - "$B$p" | tr -d '\r' | awk '!f && tolower($0) ~ /^cache-control: / { sub(/^[^:]*: /, ""); print; f = 1 }')
    [ "$cc" = "private, no-store" ] || fail "$2: $p is \"$cc\""
  done
  echo "ok - $2: headers (hashed immutable, pages no-cache, ETag per encoding, 304s, private areas no-store)"
}

# One browser run ($1: base URL, $2: label, $3/$4: colours, $5: search word).
run() {
  B=$1
  before=$(curl -fs "$B$ART" | grep -o 'css/check\.[0-9a-f]*\.css' | head -1)
  cat >"$WORK/edit-article.sh" <<EOF
set -eu
"$E" content set --site "$S" article 1 --set "title=$TITLE ($2)" >/dev/null
printf 'h1 { color: rgb($4); }\n' >"$S/static/css/check.css"
. "$WORK/wait.sh"
wait_for "$B$ART" "$TITLE ($2)" "$before"
# A page published between the edit and the reload links the plain path;
# wait for the reload's republish (the hashed name).
wait_for "$B$ART" "$TITLE ($2)" '/css/check.css"'
EOF
  cat >"$WORK/edit-search.sh" <<EOF
set -eu
"$E" content set --site "$S" article 2 --set "title=The $5 note" >/dev/null
. "$WORK/wait.sh"
wait_for "$B/search/?q=$5" "The $5 note"
EOF
  E2E_EDIT_ARTICLE="sh $WORK/edit-article.sh" E2E_EDIT_SEARCH="sh $WORK/edit-search.sh" \
    node "$TOOLS/caching-e2e.mjs" --base "$B" --article "$ART" \
    --title-before "$3" --title-after "$TITLE ($2)" \
    --color-before "rgb($6)" --color-after "rgb($4)" \
    --search "/search/?q=$5" --search-after "The $5 note"
}
# wait_for for the edit scripts.
{
  echo 'fail() { echo "not ok - $*" >&2; exit 1; }'
  sed -n '/^wait_for() {/,/^}/p' "$0"
} >"$WORK/wait.sh"
cp scripts/a11y/caching-e2e.mjs "$TOOLS/caching-e2e.mjs"

echo "== engine serve"
headers "http://127.0.0.1:$PORT" "engine serve"
run "http://127.0.0.1:$PORT" "engine" "$TITLE" "4, 5, 6" kestrel "1, 2, 3"

if [ "${HAMMERWRIGHT_CACHING_NO_CADDY:-}" = 1 ]; then
  echo "== browser caching works (engine serve; Caddy skipped)"
  exit 0
fi
echo "== Caddy (deploy/Caddyfile.publish)"
docker rm -f "$CADDY" >/dev/null 2>&1 || true
docker run -d --name "$CADDY" --network host \
  -e HAMMERWRIGHT_PORT="$CPORT" -e ENGINE_UPSTREAM="127.0.0.1:$PORT" -e OUTPUT=public \
  -v "$S:/srv/site:ro" -v "$ROOT/deploy/Caddyfile.publish:/etc/caddy/Caddyfile:ro" \
  -v "$ROOT/deploy/hammerwright-site.caddy:/etc/caddy/hammerwright-site.caddy:ro" \
  "$CADDY_IMAGE" >/dev/null
i=0
until curl -fs -o /dev/null "http://127.0.0.1:$CPORT/"; do
  i=$((i + 1)); [ "$i" -le 100 ] || { docker logs --tail 30 "$CADDY"; exit 1; }
  sleep 0.2
done
headers "http://127.0.0.1:$CPORT" "Caddy"
run "http://127.0.0.1:$CPORT" "caddy" "$TITLE (engine)" "7, 8, 9" osprey "4, 5, 6"
echo "== browser caching works (engine serve and Caddy)"
