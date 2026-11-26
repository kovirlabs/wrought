//! The `content` feature's statements on its own tables (see
//! `engine_db::sql`). Times are bound by the caller (`$n` = now).

engine_db::statements! {
    ITEM_TERMS = {
        postgres: "SELECT it.item_id, t.id, t.vocabulary, t.slug, t.name
                   FROM engine_item_terms it JOIN engine_terms t ON t.id = it.term_id
                   WHERE it.content_type = $1 AND it.item_id = ANY($2)
                   ORDER BY t.name, t.id",
        sqlite: "SELECT it.item_id, t.id, t.vocabulary, t.slug, t.name
                 FROM engine_item_terms it JOIN engine_terms t ON t.id = it.term_id
                 WHERE it.content_type = $1 AND it.item_id IN (SELECT value FROM json_each($2))
                 ORDER BY t.name, t.id",
    };
    CLEAR_ITEM_TERMS = "DELETE FROM engine_item_terms
        WHERE content_type = $1 AND item_id = $2
          AND term_id IN (SELECT id FROM engine_terms WHERE vocabulary = $3)";
    INSERT_TERM = "INSERT INTO engine_terms (vocabulary, slug, name) VALUES ($1, $2, $3)
        ON CONFLICT (vocabulary, slug) DO NOTHING";
    TERM_ID = "SELECT id FROM engine_terms WHERE vocabulary = $1 AND slug = $2";
    INSERT_ITEM_TERM = "INSERT INTO engine_item_terms (content_type, item_id, term_id)
        VALUES ($1, $2, $3) ON CONFLICT DO NOTHING";
    DELETE_ITEM_TERMS = "DELETE FROM engine_item_terms WHERE content_type = $1 AND item_id = $2";
    INSERT_REVISION = "INSERT INTO engine_revisions (content_type, item_id, version, op, data, author)
        VALUES ($1, $2, $3, $4, $5, $6)";
    REVISIONS = "SELECT id, version, op, author, data, created_at
        FROM engine_revisions WHERE content_type = $1 AND item_id = $2
        ORDER BY version DESC";
    RENAME_TERM = "UPDATE engine_terms SET name = $4, version = version + 1, updated_at = $5
        WHERE vocabulary = $1 AND id = $2 AND version = $3
        RETURNING id, vocabulary, slug, name, version";
    DELETE_TERM = "DELETE FROM engine_terms WHERE vocabulary = $1 AND id = $2 AND version = $3";
    TERM_VERSION = "SELECT version FROM engine_terms WHERE vocabulary = $1 AND id = $2";
    TERMS = "SELECT id, vocabulary, slug, name, version FROM engine_terms
        WHERE vocabulary = $1 ORDER BY name, id";
    ITEM_TERM_IDS = "SELECT term_id FROM engine_item_terms WHERE content_type = $1 AND item_id = $2";
    // Items sharing the most of the given terms (drafts are filtered by the
    // caller, which reads each candidate anyway).
    SHARING_TERMS = {
        postgres: "SELECT item_id, count(*) AS shared FROM engine_item_terms
            WHERE term_id = ANY($1) AND content_type = $2 AND item_id <> $3
            GROUP BY item_id ORDER BY shared DESC, item_id DESC LIMIT $4",
        sqlite: "SELECT item_id, count(*) AS shared FROM engine_item_terms
            WHERE term_id IN (SELECT value FROM json_each($1)) AND content_type = $2
              AND item_id <> $3
            GROUP BY item_id ORDER BY shared DESC, item_id DESC LIMIT $4",
    };
}
