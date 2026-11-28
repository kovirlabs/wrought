//! The `publish` feature's statements (see `engine_db::sql`).

engine_db::statements! {
    LOOKUP = "SELECT DISTINCT path FROM engine_page_tags WHERE tag = ANY($1) ORDER BY path";
    TAGS_OF = "SELECT tag FROM engine_page_tags WHERE path = $1 ORDER BY tag";
    PAGE_SHA256 = "SELECT sha256 FROM engine_pages WHERE path = $1";
    ALL_PAGES ="SELECT path, output FROM engine_pages ORDER BY path";
    REMOVE_PAGE = "DELETE FROM engine_pages WHERE path = $1";
    CLEAR_PAGES = "DELETE FROM engine_pages";
    // `engine build` commits the tag index without waiting for the WAL to
    // reach the disk, like the pages it describes (written, not fsynced).
    ASYNC_COMMIT = "SET LOCAL synchronous_commit = off";
    SET_CURSOR = "UPDATE engine_publish_state SET last_change = $1";
    LATEST_CHANGE = "SELECT COALESCE(max(id), 0) FROM engine_changes";
    CURSOR = "SELECT last_change FROM engine_publish_state";
    PENDING = "SELECT id, content_type, item_id, op FROM engine_changes
        WHERE id > $1 ORDER BY id LIMIT $2";
    ADVANCE = "UPDATE engine_publish_state SET last_change = GREATEST(last_change, $1)";
    PRUNE_CHANGES = "DELETE FROM engine_changes WHERE id <= $1";
    UPSERT_PAGE = "INSERT INTO engine_pages (path, output, sha256, rendered_at) VALUES ($1, $2, $3, $4)
        ON CONFLICT (path) DO UPDATE
          SET output = excluded.output, sha256 = excluded.sha256, rendered_at = excluded.rendered_at";
    CLEAR_PAGE_TAGS = "DELETE FROM engine_page_tags WHERE path = $1";
    INSERT_PAGE_TAGS = "INSERT INTO engine_page_tags (tag, path) SELECT DISTINCT unnest($2::text[]), $1";
    // `engine build` writes the whole index at once: three statements for
    // any number of pages instead of three per page (parallel arrays).
    BULK_PAGES = "INSERT INTO engine_pages (path, output, sha256, rendered_at)
            SELECT p, o, h, CAST($4 AS TIMESTAMPTZ)
            FROM unnest(CAST($1 AS TEXT[]), CAST($2 AS TEXT[]), CAST($3 AS TEXT[])) AS x(p, o, h)";
    // A publish batch: every page's entry in three statements instead of
    // three per page.
    UPSERT_PAGES = "INSERT INTO engine_pages (path, output, sha256, rendered_at)
            SELECT p, o, h, CAST($4 AS TIMESTAMPTZ)
            FROM unnest(CAST($1 AS TEXT[]), CAST($2 AS TEXT[]), CAST($3 AS TEXT[])) AS x(p, o, h)
            ON CONFLICT (path) DO UPDATE
              SET output = excluded.output, sha256 = excluded.sha256,
                  rendered_at = excluded.rendered_at";
    CLEAR_TAGS_OF = "DELETE FROM engine_page_tags WHERE path = ANY(CAST($1 AS TEXT[]))";
    BULK_PAGE_TAGS = "INSERT INTO engine_page_tags (tag, path)
            SELECT DISTINCT t, p FROM unnest(CAST($1 AS TEXT[]), CAST($2 AS TEXT[])) AS x(t, p)";
}
