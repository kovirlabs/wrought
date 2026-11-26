# Shared by scripts/test-postgres.sh and scripts/check-queries.sh (sourced).

psql_c() { docker exec -i wrought-postgres psql -q -v ON_ERROR_STOP=1 -U wrought "$@"; }

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
    if docker exec -e PGPASSWORD=wrought wrought-postgres \
        psql -h 127.0.0.1 -U wrought -d wrought -Atc 'SELECT 1' >/dev/null 2>&1; then
      ok=$((ok + 1))
    else
      ok=0
    fi
    i=$((i + 1))
    if [ "$i" -gt 120 ]; then
      echo "wrought-postgres did not become ready" >&2
      docker logs --tail 30 wrought-postgres >&2
      exit 1
    fi
    sleep 0.25
  done
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
