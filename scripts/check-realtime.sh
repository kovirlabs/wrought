#!/bin/sh
# Live regions end to end in a real browser (DECISIONS.md, "Real-time"):
# builds the engine (cms preset plus `realtime`), copies the cms example
# into target/realtime-work on PostgreSQL with a live region on the article
# list, serves it, and runs scripts/a11y/realtime-e2e.mjs in headless
# Chromium twice: straight against `engine serve`, then through the
# shipped deploy/Caddyfile.publish (Caddy in a `wrought-test-realtime`
# container on the host network, removed afterwards), which must pass the
# event stream through without holding events back. Each run edits an
# article and waits for the region to show it without a reload.
#
#   scripts/check-realtime.sh
#
# Uses the Playwright install of scripts/check-a11y.sh (target/a11y-tools;
# run that once first, or set HAMMERWRIGHT_A11Y_TOOLS). Ports 59450
# (engine, HAMMERWRIGHT_REALTIME_PORT) and 59451 (Caddy,
# HAMMERWRIGHT_REALTIME_CADDY_PORT). HAMMERWRIGHT_REALTIME_NO_CADDY=1 skips
# the Caddy run. The site's database is a throwaway one on
# wrought-test-postgres (started if needed), or on the server
# HAMMERWRIGHT_PG_URL names (scripts/lib.sh, `check_db`).
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
TOOLS=${HAMMERWRIGHT_A11Y_TOOLS:-$ROOT/target/a11y-tools}
WORK=$ROOT/target/realtime-work
PORT=${HAMMERWRIGHT_REALTIME_PORT:-59450}
CPORT=${HAMMERWRIGHT_REALTIME_CADDY_PORT:-59451}
CADDY_IMAGE=caddy:2.11.8-alpine
CADDY=wrought-test-realtime
. scripts/lib.sh
[ -f "$TOOLS/node_modules/playwright/package.json" ] ||
  { echo "Playwright is not installed in $TOOLS: run scripts/check-a11y.sh once" >&2; exit 2; }
require_free_port "$PORT"

echo "== building the engine (cms preset, realtime)"
cargo build -q -p engine-cli --features cms,realtime
E=$ROOT/target/debug/engine
S=$WORK/site

rm -rf "$WORK"
mkdir -p "$S"
(cd examples/cms-site && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$S" && tar -xf -)
# The article list gets a live region following every article.
cat >"$WORK/region.html" <<'EOR'
{% set count %}<p>{{ listing.items | length }} articles: {% for a in listing.items %}{{ a.title }}; {% endfor %}</p>{% endset %}
{{ component("live_region", id="latest", tags=["c:article"], body=count, label="Latest articles") }}
EOR
sed -i '/^<h1>/r '"$WORK/region.html" "$S/templates/articles.html"

check_pg_ready
SERVER=""
cleanup() {
  stop_pid "$SERVER"
  docker rm -f "$CADDY" >/dev/null 2>&1 || true
  drop_check_dbs
}
trap cleanup EXIT
trap 'exit 130' INT TERM
check_db realtime
ENGINE_DATABASE_URL=$CHECK_DB_URL
export ENGINE_DATABASE_URL
"$E" migrate --site "$S" >/dev/null
"$E" content seed --site "$S" >/dev/null
"$E" build --site "$S" >/dev/null
TITLE=$("$E" content show --site "$S" article 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const j=JSON.parse(s);console.log(j.title ?? j.fields.title)})')

"$E" serve --site "$S" --port "$PORT" --no-workers >"$WORK/serve.log" 2>&1 &
SERVER=$!
i=0
until curl -fs -o /dev/null "http://127.0.0.1:$PORT/_engine/health"; do
  i=$((i + 1)); [ "$i" -le 100 ] || { cat "$WORK/serve.log"; exit 1; }
  sleep 0.2
done
cp scripts/a11y/realtime-e2e.mjs "$TOOLS/realtime-e2e.mjs"

# One browser run ($1: base URL, $2: label).
run() {
  E2E_EDIT="\"$E\" content set --site \"$S\" article 1 --set \"title=$TITLE ($2)\" >/dev/null" \
    node "$TOOLS/realtime-e2e.mjs" --base "$1" --path /articles/ --region latest \
    --after "$TITLE ($2)"
}

echo "== engine serve"
run "http://127.0.0.1:$PORT" "engine"

if [ "${HAMMERWRIGHT_REALTIME_NO_CADDY:-}" = 1 ]; then
  echo "== live regions work (engine serve; Caddy skipped)"
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
run "http://127.0.0.1:$CPORT" "caddy"
echo "== live regions work (engine serve and Caddy)"
