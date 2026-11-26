-- Wrought `search` feature, PostgreSQL: one row per published item, with a
-- generated tsvector (title weighted above body) and a GIN index.
CREATE TABLE engine_search (
    id bigserial PRIMARY KEY,
    content_type text NOT NULL,
    item_id bigint NOT NULL,
    title text NOT NULL DEFAULT '',
    body text NOT NULL DEFAULT '',
    path text,
    published_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- Text search configuration (`[search] language`), e.g. english, simple.
    lang regconfig NOT NULL DEFAULT 'english',
    tsv tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector(lang, title), 'A') || setweight(to_tsvector(lang, body), 'B')
    ) STORED,
    UNIQUE (content_type, item_id)
);
CREATE INDEX engine_search_tsv ON engine_search USING gin (tsv);
