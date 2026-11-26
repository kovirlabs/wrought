-- Wrought `publish` feature: the tag index and the change-log cursor.

-- Every published file (one row per URL path) ...
CREATE TABLE engine_pages (
    path text PRIMARY KEY,
    output text NOT NULL,
    sha256 text NOT NULL,
    rendered_at timestamptz NOT NULL DEFAULT now()
);

-- ... and the cache tags its last render recorded. A change to tag T
-- re-renders exactly the paths listed under T.
CREATE TABLE engine_page_tags (
    tag text NOT NULL,
    path text NOT NULL REFERENCES engine_pages (path) ON DELETE CASCADE,
    PRIMARY KEY (tag, path)
);
CREATE INDEX engine_page_tags_path ON engine_page_tags (path);

-- How far the publisher has read engine_changes.
CREATE TABLE engine_publish_state (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    last_change bigint NOT NULL DEFAULT 0
);
INSERT INTO engine_publish_state DEFAULT VALUES;
