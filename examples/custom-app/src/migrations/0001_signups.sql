-- The site's own tables, embedded in the binary (App::migration) and
-- applied by `migrate` as `app/0001_signups`.
CREATE TABLE app_signups (
    id bigserial PRIMARY KEY,
    email text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    welcomed_at timestamptz
);

-- Feed the engine's change log as `signups`: every change publishes the
-- pages that used a query tagged `c:signups` (or `i:signups:<id>`), drops
-- cached fragments carrying those tags and calls the publish hooks.
CREATE TRIGGER app_signups_changes
    AFTER INSERT OR UPDATE OR DELETE ON app_signups
    FOR EACH ROW EXECUTE FUNCTION engine_record_change('signups');

-- Written by the publish hook.
CREATE TABLE app_publish_log (
    id bigserial PRIMARY KEY,
    path text NOT NULL,
    reason text NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
