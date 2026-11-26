//! The `publish` feature's statements (see `engine_db::sql`).

engine_db::statements! {
    LOOKUP = {
        postgres: "SELECT DISTINCT path FROM engine_page_tags WHERE tag = ANY($1) ORDER BY path",
        sqlite: "SELECT DISTINCT path FROM engine_page_tags
                 WHERE tag IN (SELECT value FROM json_each($1)) ORDER BY path",
    };
    TAGS_OF = "SELECT tag FROM engine_page_tags WHERE path = $1 ORDER BY tag";
    ALL_PAGES = "SELECT path, output FROM engine_pages ORDER BY path";
    REMOVE_PAGE = "DELETE FROM engine_pages WHERE path = $1";
    CLEAR_PAGES = "DELETE FROM engine_pages";
    SET_CURSOR = "UPDATE engine_publish_state SET last_change = $1";
    LATEST_CHANGE = "SELECT COALESCE(max(id), 0) FROM engine_changes";
    CURSOR = "SELECT last_change FROM engine_publish_state";
    PENDING = "SELECT id, content_type, item_id, op FROM engine_changes
        WHERE id > $1 ORDER BY id LIMIT $2";
    ADVANCE = {
        postgres: "UPDATE engine_publish_state SET last_change = GREATEST(last_change, $1)",
        sqlite: "UPDATE engine_publish_state SET last_change = max(last_change, $1)",
    };
    PRUNE_CHANGES = "DELETE FROM engine_changes WHERE id <= $1";
    UPSERT_PAGE = "INSERT INTO engine_pages (path, output, sha256, rendered_at) VALUES ($1, $2, $3, $4)
        ON CONFLICT (path) DO UPDATE
          SET output = excluded.output, sha256 = excluded.sha256, rendered_at = excluded.rendered_at";
    CLEAR_PAGE_TAGS = "DELETE FROM engine_page_tags WHERE path = $1";
    INSERT_PAGE_TAGS = {
        postgres: "INSERT INTO engine_page_tags (tag, path) SELECT DISTINCT unnest($2::text[]), $1",
        sqlite: "INSERT INTO engine_page_tags (tag, path) SELECT DISTINCT value, $1 FROM json_each($2)",
    };
}
