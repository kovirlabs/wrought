#!/bin/sh
# The HTTPS front door (deploy/Caddyfile.tls with front-door.caddy and
# wrought-site.caddy) as shipped: TLS versions and cipher suites, HTTP/2
# and HTTP/3, HSTS and the security headers, and that the engine's own
# stricter headers (sign-in, admin, the visual editor's frame) survive.
#
# Starts `engine serve` on the cms example (SQLite, no container) and Caddy
# in a `wrought-test-front-door` container (removed afterwards) on the
# host network, bound to 127.0.0.1 on high ports. A test-only global.d
# file switches Caddy to its internal certificate authority (no ACME, no
# public calls); its keys live in the container's tmpfs and go with it.
# Probes with the host's openssl and curl.
#
#   scripts/check-front-door.sh
#
# Ports: 59443 HTTPS and HTTP/3 (WROUGHT_TLS_PORT), 59480 HTTP
# (WROUGHT_TLS_HTTP_PORT), 59440 the engine (WROUGHT_TLS_ENGINE_PORT).
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
WORK=$ROOT/target/front-door-work
TLS_PORT=${WROUGHT_TLS_PORT:-59443}
HTTP_PORT=${WROUGHT_TLS_HTTP_PORT:-59480}
EPORT=${WROUGHT_TLS_ENGINE_PORT:-59440}
CADDY_IMAGE=caddy:2.11.8-alpine
NAME=wrought-test-front-door
command -v openssl >/dev/null || { echo "needs openssl" >&2; exit 2; }

echo "== building the engine (cms preset)"
cargo build -q -p engine-cli --features cms
E=$ROOT/target/debug/engine
S=$WORK/site
rm -rf "$WORK"
mkdir -p "$S"
(cd examples/cms-site && tar --exclude=./public --exclude=./data --exclude='./.public*' -cf - .) | (cd "$S" && tar -xf -)
ENGINE_DATABASE_URL="sqlite://$S/site.db"
export ENGINE_DATABASE_URL
"$E" migrate --site "$S" >/dev/null
"$E" content seed --site "$S" >/dev/null
"$E" build --site "$S" >/dev/null

# Test-only global options: internal CA, no trust store changes, high
# ports, loopback only.
mkdir -p "$WORK/global.d"
cat >"$WORK/global.d/test.caddy" <<EOF
local_certs
skip_install_trust
http_port $HTTP_PORT
https_port $TLS_PORT
default_bind 127.0.0.1
EOF

"$E" serve --site "$S" --port "$EPORT" --no-workers >"$WORK/serve.log" 2>&1 &
SERVER=$!
cleanup() {
  kill "$SERVER" 2>/dev/null || true
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --network host --read-only --tmpfs /data --tmpfs /config \
  -e WROUGHT_DOMAIN=localhost -e ENGINE_UPSTREAM="127.0.0.1:$EPORT" -e OUTPUT=public \
  -v "$ROOT/deploy/Caddyfile.tls:/etc/caddy/Caddyfile:ro" \
  -v "$ROOT/deploy/front-door.caddy:/etc/caddy/front-door.caddy:ro" \
  -v "$ROOT/deploy/wrought-site.caddy:/etc/caddy/wrought-site.caddy:ro" \
  -v "$WORK/global.d:/etc/caddy/global.d:ro" \
  -v "$S:/srv/site:ro" \
  "$CADDY_IMAGE" >/dev/null
B=https://localhost:$TLS_PORT
i=0
until curl -fsk -o /dev/null "$B/" 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -le 150 ] || { docker logs --tail 40 "$NAME"; tail -20 "$WORK/serve.log"; exit 1; }
  sleep 0.2
done

n=0
ok() { n=$((n + 1)); echo "ok $n - $*"; }
fail() { echo "not ok - $*" >&2; docker logs --tail 20 "$NAME" >&2; exit 1; }

# One handshake; prints openssl's output. Extra arguments go to s_client.
hs() { openssl s_client -connect "127.0.0.1:$TLS_PORT" -servername localhost "$@" </dev/null 2>&1 || true; }
proto() { printf '%s\n' "$1" | sed -n 's/^ *Protocol *: *//p' | head -1; }
cipher() { printf '%s\n' "$1" | sed -n 's/^ *Cipher *: *//p' | head -1; }
# A handshake the server refused: the client sent its hello and got an
# alert back (not a client-side refusal such as "no ciphers available").
refused() {
  printf '%s' "$1" | grep -qE 'alert (handshake failure|protocol version)|tlsv1 alert|sslv3 alert' &&
    ! printf '%s' "$1" | grep -qE 'New, TLSv|Cipher is [A-Z]'
}

out=$(hs)
[ "$(proto "$out")" = TLSv1.3 ] || fail "default handshake is $(proto "$out")"
ok "a modern client gets TLS 1.3 ($(cipher "$out"))"
for c in TLS_AES_128_GCM_SHA256 TLS_AES_256_GCM_SHA384 TLS_CHACHA20_POLY1305_SHA256; do
  out=$(hs -tls1_3 -ciphersuites "$c")
  [ "$(cipher "$out")" = "$c" ] || fail "TLS 1.3 $c: $(cipher "$out")"
done
ok "TLS 1.3 with AES-128-GCM, AES-256-GCM and ChaCha20-Poly1305"

cert=$(hs | openssl x509 -noout -text 2>/dev/null || true)
printf '%s' "$cert" | grep -q 'id-ecPublicKey' && printf '%s' "$cert" | grep -q 'P-256' ||
  fail "certificate is not ECDSA P-256"
ok "the certificate is ECDSA P-256"

for c in ECDHE-ECDSA-AES128-GCM-SHA256 ECDHE-ECDSA-AES256-GCM-SHA384 ECDHE-ECDSA-CHACHA20-POLY1305; do
  out=$(hs -tls1_2 -cipher "$c")
  [ "$(proto "$out")" = TLSv1.2 ] && [ "$(cipher "$out")" = "$c" ] || fail "TLS 1.2 $c refused: $out"
done
ok "TLS 1.2 with ECDHE and AEAD suites only is accepted"

for c in ECDHE-ECDSA-AES128-SHA ECDHE-ECDSA-AES256-SHA ECDHE-ECDSA-AES128-SHA256 \
  ECDHE-ECDSA-AES256-SHA384 AES128-GCM-SHA256 AES256-GCM-SHA384 AES128-SHA AES256-SHA; do
  out=$(hs -tls1_2 -cipher "$c:@SECLEVEL=0")
  refused "$out" || fail "TLS 1.2 $c was not refused: $(printf '%s' "$out" | tail -5)"
done
ok "TLS 1.2 CBC, SHA-1 and RSA key exchange suites are refused"

for v in -tls1 -tls1_1; do
  out=$(hs "$v" -cipher 'DEFAULT:@SECLEVEL=0')
  printf '%s' "$out" | grep -q 'no protocols available' && fail "the client cannot offer $v here"
  refused "$out" || fail "$v was not refused: $(printf '%s' "$out" | tail -5)"
done
ok "TLS 1.0 and 1.1 are refused"

# HTTP: version, redirect, headers.
h() { curl -sk -o /dev/null -D - "$@" | tr -d '\r'; }
hv() { printf '%s\n' "$1" | awk -v n="$2" '!f && tolower($0) ~ "^"n": " { sub(/^[^:]*: /, ""); print; f = 1 }'; }
[ "$(curl -sk -o /dev/null -w '%{http_version}' "$B/")" = 2 ] || fail "no HTTP/2"
ok "HTTP/2 over TLS"
page=$(h "$B/")
alt=$(hv "$page" alt-svc)
printf '%s' "$alt" | grep -q "h3=\":$TLS_PORT\"" || fail "no HTTP/3 in Alt-Svc ($alt)"
docker logs "$NAME" 2>&1 | grep -q 'enabling HTTP/3 listener' || fail "no HTTP/3 listener in Caddy's log"
ok "HTTP/3 listens and is announced ($alt)"
r=$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "http://localhost:$HTTP_PORT/articles/")
# (Caddy's redirect names the standard port; this test moved it, so only
# the status, scheme, host and path are checked.)
case $r in "308 https://localhost"*/articles/) ;; *) fail "http:// redirect: $r" ;; esac
ok "http:// redirects to https:// ($r)"

hsts=$(hv "$page" strict-transport-security)
[ "$hsts" = "max-age=31536000" ] || fail "HSTS is \"$hsts\""
ok "HSTS for a year, no includeSubDomains, no preload"
for pair in "x-content-type-options=nosniff" "referrer-policy=strict-origin-when-cross-origin" \
  "x-frame-options=SAMEORIGIN" "content-security-policy=frame-ancestors 'self'" \
  "permissions-policy=camera=(), microphone=(), geolocation=(), browsing-topics=()" \
  "cache-control=no-cache"; do
  k=${pair%%=*}; v=${pair#*=}
  [ "$(hv "$page" "$k")" = "$v" ] || fail "published page: $k is \"$(hv "$page" "$k")\", not \"$v\""
done
[ -z "$(hv "$page" server)" ] || fail "Server header sent"
ok "security headers on a published page"

# The engine's own headers win: the sign-in page keeps its strict policy.
login=$(h "$B/account/login/")
csp=$(hv "$login" content-security-policy)
printf '%s' "$csp" | grep -q "frame-ancestors 'none'" || fail "sign-in CSP replaced: $csp"
[ "$(printf '%s\n' "$login" | grep -ci '^content-security-policy:')" = 1 ] || fail "two CSP headers on sign-in"
[ "$(hv "$login" x-frame-options)" = DENY ] || fail "sign-in X-Frame-Options: $(hv "$login" x-frame-options)"
[ "$(hv "$login" referrer-policy)" = same-origin ] || fail "sign-in Referrer-Policy: $(hv "$login" referrer-policy)"
[ "$(hv "$login" cache-control)" = "private, no-store" ] || fail "sign-in Cache-Control: $(hv "$login" cache-control)"
[ "$(hv "$login" strict-transport-security)" = "max-age=31536000" ] || fail "no HSTS on engine pages"
ok "the engine's stricter headers on its own pages are kept (CSP, frame, referrer)"
echo "== the HTTPS front door is configured as shipped ($n checks)"
