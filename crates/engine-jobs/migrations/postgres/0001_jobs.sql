-- Wrought `jobs` feature: a job queue and schedules on Postgres.
--
-- Workers claim jobs with `FOR UPDATE SKIP LOCKED`, so any number of
-- workers (threads or processes) can share the queue without running a job
-- twice. A claimed job holds a lease (`locked_until`) that its worker keeps
-- extending while it runs; a job whose lease ran out belongs to a worker
-- that crashed or lost its connection, and is put back in the queue.

CREATE TABLE engine_jobs (
    id bigserial PRIMARY KEY,
    -- Handler name, e.g. `mail.send`.
    kind text NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_.-]{0,63}$'),
    payload jsonb NOT NULL DEFAULT '{}',
    -- queued -> running -> done; on failure back to queued (with backoff)
    -- until max_attempts, then dead. Dead jobs stay for inspection and can
    -- be retried by hand.
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'done', 'dead')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 5 CHECK (max_attempts BETWEEN 1 AND 100),
    run_at timestamptz NOT NULL DEFAULT now(),
    locked_by text,
    locked_until timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz
);
-- The claim query: oldest due queued job.
CREATE INDEX engine_jobs_due ON engine_jobs (run_at, id) WHERE status = 'queued';
-- The lease reaper.
CREATE INDEX engine_jobs_running ON engine_jobs (locked_until) WHERE status = 'running';
CREATE INDEX engine_jobs_status ON engine_jobs (status, id);

-- Recurring jobs from `[[jobs.schedule]]` (and the engine's own). Each
-- process syncs the list at start; claiming a due schedule is one locked
-- UPDATE, so several processes enqueue it once.
CREATE TABLE engine_job_schedules (
    name text PRIMARY KEY CHECK (name ~ '^[a-z][a-z0-9_.-]{0,63}$'),
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    every_seconds integer NOT NULL CHECK (every_seconds >= 1),
    next_run_at timestamptz NOT NULL,
    last_run_at timestamptz
);

-- Wake workers up when a job is queued (they also poll, so a lost
-- notification only delays a job by the poll interval).
CREATE FUNCTION engine_jobs_notify() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('engine_jobs', NEW.kind);
    RETURN NULL;
END $$;
CREATE TRIGGER engine_jobs_notify AFTER INSERT ON engine_jobs
    FOR EACH ROW WHEN (NEW.status = 'queued') EXECUTE FUNCTION engine_jobs_notify();
