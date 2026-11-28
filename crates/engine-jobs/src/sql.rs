//! The `jobs` feature's statements: PostgreSQL only (the spec keeps jobs on
//! Postgres for `FOR UPDATE SKIP LOCKED` and `LISTEN/NOTIFY`).

engine_db::statements! {
    NEXT_DUE = { postgres: "SELECT extract(epoch FROM least(
                (SELECT min(next_run_at) FROM engine_job_schedules WHERE kind = ANY($1)),
                (SELECT min(run_at) FROM engine_jobs
                  WHERE status = 'queued' AND kind = ANY($1))))::bigint" };
    JOB = { postgres: "SELECT id, kind, status, attempts, max_attempts, run_at, locked_by,
                last_error, created_at, finished_at, payload
            FROM engine_jobs WHERE id = $1" };
    LIST = { postgres: "SELECT id, kind, status, attempts, max_attempts, run_at, locked_by,
                last_error, created_at, finished_at, payload
            FROM engine_jobs WHERE ($1::text IS NULL OR status = $1)
            ORDER BY id DESC LIMIT $2 OFFSET $3" };
    COUNTS = { postgres: "SELECT status, count(*) FROM engine_jobs GROUP BY status ORDER BY status" };
    RETRY = { postgres: "UPDATE engine_jobs SET status = 'queued', attempts = 0, run_at = now(),
            finished_at = NULL WHERE id = $1 AND status = 'dead'" };
    DELETE = { postgres: "DELETE FROM engine_jobs WHERE id = $1 AND status <> 'running'" };
    PRUNE = { postgres: "DELETE FROM engine_jobs
            WHERE (status = 'done' AND finished_at < now() - make_interval(secs => $1))
               OR (status = 'dead' AND finished_at < now() - make_interval(secs => $2))" };
    ENQUEUE = { postgres: "INSERT INTO engine_jobs (kind, payload, max_attempts, run_at)
            VALUES ($1, $2, $3, now() + make_interval(secs => $4)) RETURNING id" };
    CLAIM = { postgres: "UPDATE engine_jobs SET status = 'running', attempts = attempts + 1,
                locked_by = $1, locked_until = now() + make_interval(secs => $2),
                started_at = now()
            WHERE id = (
                SELECT id FROM engine_jobs
                WHERE status = 'queued' AND run_at <= now() AND kind = ANY($3)
                ORDER BY run_at, id
                LIMIT 1
                FOR UPDATE SKIP LOCKED)
            RETURNING id, kind, payload, attempts, max_attempts" };
    HEARTBEAT = { postgres: "UPDATE engine_jobs SET locked_until = now() + make_interval(secs => $3)
            WHERE id = $1 AND locked_by = $2 AND status = 'running'" };
    FINISH = { postgres: "UPDATE engine_jobs SET status = 'done', finished_at = now(), locked_by = NULL,
                locked_until = NULL, last_error = NULL,
                payload = CASE WHEN $3 THEN '{}'::jsonb ELSE payload END
            WHERE id = $1 AND locked_by = $2 AND status = 'running'" };
    FAIL = { postgres: "UPDATE engine_jobs SET
                status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
                finished_at = CASE WHEN attempts >= max_attempts THEN now() END,
                run_at = now() + make_interval(secs => $3),
                last_error = $4, locked_by = NULL, locked_until = NULL
            WHERE id = $1 AND locked_by = $2 AND status = 'running'" };
    REAP = { postgres: "UPDATE engine_jobs SET
                status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
                finished_at = CASE WHEN attempts >= max_attempts THEN now() END,
                run_at = now() + make_interval(secs => least($1 * power(2, greatest(attempts - 1, 0)), $2)),
                last_error = 'the worker running it stopped before finishing (lease expired; worker '
                             || coalesce(locked_by, '?') || ')',
                locked_by = NULL, locked_until = NULL
            WHERE status = 'running' AND locked_until < now()
            RETURNING id, status" };
    RELEASE = { postgres: "UPDATE engine_jobs SET status = 'queued', attempts = greatest(attempts - 1, 0),
                run_at = now(), locked_by = NULL, locked_until = NULL
            WHERE id = $1 AND locked_by = $2 AND status = 'running'" };
    DROP_SCHEDULES = { postgres: "DELETE FROM engine_job_schedules WHERE NOT (name = ANY($1))" };
    UPSERT_SCHEDULE = { postgres: "INSERT INTO engine_job_schedules (name, kind, payload, every_seconds, next_run_at)
            VALUES ($1, $2, $3, $4, now() + make_interval(secs => $4::int4::float8))
            ON CONFLICT (name) DO UPDATE SET kind = EXCLUDED.kind, payload = EXCLUDED.payload,
                every_seconds = EXCLUDED.every_seconds,
                next_run_at = CASE WHEN engine_job_schedules.every_seconds = EXCLUDED.every_seconds
                                   THEN engine_job_schedules.next_run_at
                                   ELSE EXCLUDED.next_run_at END" };
    DUE_SCHEDULES = { postgres: "UPDATE engine_job_schedules s SET last_run_at = now(),
                next_run_at = now() + make_interval(secs => s.every_seconds::float8)
            WHERE s.name IN (SELECT name FROM engine_job_schedules
                             WHERE next_run_at <= now() FOR UPDATE SKIP LOCKED)
            RETURNING s.name, s.kind, s.payload" };
}
