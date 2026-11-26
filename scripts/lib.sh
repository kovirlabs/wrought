# Shared by scripts/test-postgres.sh and scripts/check-queries.sh (sourced).

psql_c() { docker exec -i wrought-test-postgres psql -q -v ON_ERROR_STOP=1 -U wrought "$@"; }

# Start the container and wait until the *real* server answers a query over
# TCP on the published port. `docker compose --wait` alone is not enough on
# a fresh volume: the image's initdb step runs a temporary server (Unix
# socket only) and restarts it, and anything that connects in that window
# fails ("the database system is shutting down").
start_postgres() {
  docker compose -f deploy/compose.postgres.yml up -d --wait
  i=0
  ok=0
  while [ "$ok" -lt 3 ]; do
    if docker exec -e PGPASSWORD=wrought wrought-test-postgres \
        psql -h 127.0.0.1 -U wrought -d wrought -Atc 'SELECT 1' >/dev/null 2>&1; then
      ok=$((ok + 1))
    else
      ok=0
    fi
    i=$((i + 1))
    if [ "$i" -gt 120 ]; then
      echo "wrought-test-postgres did not become ready" >&2
      docker logs --tail 30 wrought-test-postgres >&2
      exit 1
    fi
    sleep 0.25
  done
}

# Start the MinIO container (S3 tests) and wait until it answers.
start_minio() {
  docker compose -f deploy/compose.minio.yml up -d --wait
}

# The variables the S3 tests read, for the container's public test
# credentials (deploy/compose.minio.yml).
minio_env() {
  echo "ENGINE_TEST_S3_ENDPOINT=http://127.0.0.1:${WROUGHT_MINIO_PORT:-59000}"
  echo "ENGINE_TEST_S3_ACCESS_KEY_ID=wrought-test"
  echo "ENGINE_TEST_S3_SECRET_ACCESS_KEY=wrought-test-secret"
}

# Whether the MinIO container answers.
minio_up() {
  curl -fs -o /dev/null "http://127.0.0.1:${WROUGHT_MINIO_PORT:-59000}/minio/health/live" 2>/dev/null
}

# Drop `wrought_test_<pid>_*` databases whose test process no longer runs.
drop_stale_test_databases() {
  dbs=$(psql_c -d postgres -Atc \
    "SELECT datname FROM pg_database WHERE datname LIKE 'wrought\_test\_%'")
  for db in $dbs; do
    pid=$(echo "$db" | cut -d_ -f3)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      continue
    fi
    psql_c -d postgres -c "DROP DATABASE IF EXISTS \"$db\" WITH (FORCE)"
  done
}

# Start the local OpenID Connect provider (Dex, deploy/compose.oidc.yml)
# for the `oidc` end-to-end test and export ENGINE_TEST_OIDC_ISSUER. Set
# WROUGHT_NO_OIDC=1 to skip it (the test is then skipped too).
start_oidc() {
  if [ "${WROUGHT_NO_OIDC:-}" = "1" ]; then
    return 0
  fi
  docker compose -f deploy/compose.oidc.yml up -d --wait
  ENGINE_TEST_OIDC_ISSUER="http://127.0.0.1:59110/dex"
  export ENGINE_TEST_OIDC_ISSUER
}
