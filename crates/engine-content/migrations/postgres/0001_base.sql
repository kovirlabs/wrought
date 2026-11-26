-- Wrought `content` feature: tables shared by every content type.
-- Per-type tables (content_<type>) come from the site's migrations, which
-- `engine types sync` generates from types/*.toml.

-- Every save of an item writes one revision row, in the save's transaction.
CREATE TABLE engine_revisions (
    id bigserial PRIMARY KEY,
    content_type text NOT NULL,
    item_id bigint NOT NULL,
    version integer NOT NULL,
    op text NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    data jsonb NOT NULL,
    author text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (content_type, item_id, version)
);

-- Taxonomy terms, grouped by vocabulary (named by `terms` fields).
CREATE TABLE engine_terms (
    id bigserial PRIMARY KEY,
    vocabulary text NOT NULL,
    slug text NOT NULL CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 80),
    name text NOT NULL,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (vocabulary, slug)
);

CREATE TABLE engine_item_terms (
    content_type text NOT NULL,
    item_id bigint NOT NULL,
    term_id bigint NOT NULL REFERENCES engine_terms (id) ON DELETE CASCADE,
    PRIMARY KEY (content_type, item_id, term_id)
);
CREATE INDEX engine_item_terms_term ON engine_item_terms (term_id);

-- Change log: one row per changed item or term, written by triggers (so
-- edits made directly in SQL are seen too). The publish listener reads it
-- after each NOTIFY on channel `engine_changes`, and after reconnecting, so
-- no change is lost while the listener is down.
CREATE TABLE engine_changes (
    id bigserial PRIMARY KEY,
    content_type text NOT NULL,
    item_id bigint NOT NULL,
    op text NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);

-- Trigger for content_<type> tables; TG_ARGV[0] is the type name.
CREATE FUNCTION engine_record_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    change_id bigint;
    row_id bigint;
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_id := OLD.id;
    ELSE
        row_id := NEW.id;
    END IF;
    INSERT INTO engine_changes (content_type, item_id, op)
        VALUES (TG_ARGV[0], row_id, TG_OP)
        RETURNING id INTO change_id;
    PERFORM pg_notify('engine_changes', change_id::text);
    RETURN NULL;
END
$$;

-- Terms: the vocabulary is the "type" of a term change.
CREATE FUNCTION engine_record_term_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    change_id bigint;
    r engine_terms;
BEGIN
    IF TG_OP = 'DELETE' THEN
        r := OLD;
    ELSE
        r := NEW;
    END IF;
    INSERT INTO engine_changes (content_type, item_id, op)
        VALUES (r.vocabulary, r.id, TG_OP)
        RETURNING id INTO change_id;
    PERFORM pg_notify('engine_changes', change_id::text);
    RETURN NULL;
END
$$;

CREATE TRIGGER engine_terms_changes
    AFTER INSERT OR UPDATE OR DELETE ON engine_terms
    FOR EACH ROW EXECUTE FUNCTION engine_record_term_change();

-- Term assignments change the item they belong to.
CREATE FUNCTION engine_record_item_terms_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    change_id bigint;
    r engine_item_terms;
BEGIN
    IF TG_OP = 'DELETE' THEN
        r := OLD;
    ELSE
        r := NEW;
    END IF;
    INSERT INTO engine_changes (content_type, item_id, op)
        VALUES (r.content_type, r.item_id, 'UPDATE')
        RETURNING id INTO change_id;
    PERFORM pg_notify('engine_changes', change_id::text);
    RETURN NULL;
END
$$;

CREATE TRIGGER engine_item_terms_changes
    AFTER INSERT OR DELETE ON engine_item_terms
    FOR EACH ROW EXECUTE FUNCTION engine_record_item_terms_change();
