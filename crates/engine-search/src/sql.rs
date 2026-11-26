//! The `search` feature's statements (see `engine_db::sql`). Both
//! backends take the same parameters: `$1` is the Postgres text search
//! configuration (unused by SQLite, whose tokenizer is fixed in the table).

engine_db::statements! {
    UPSERT = {
        postgres: "INSERT INTO engine_search
                (content_type, item_id, title, body, path, published_at, updated_at, lang)
            VALUES ($2, $3, $4, $5, $6, $7, $8, $1::regconfig)
            ON CONFLICT (content_type, item_id) DO UPDATE SET
                title = excluded.title, body = excluded.body, path = excluded.path,
                published_at = excluded.published_at, updated_at = excluded.updated_at,
                lang = excluded.lang",
        sqlite: "INSERT INTO engine_search
                (content_type, item_id, title, body, path, published_at, updated_at)
            VALUES ($2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (content_type, item_id) DO UPDATE SET
                title = excluded.title, body = excluded.body, path = excluded.path,
                published_at = excluded.published_at, updated_at = excluded.updated_at",
    };
    REMOVE = "DELETE FROM engine_search WHERE content_type = $1 AND item_id = $2";
    CLEAR = "DELETE FROM engine_search";
    COUNT_DOCS = "SELECT count(*) FROM engine_search";
    // Highlights come back between \x01 and \x02 (control characters are
    // removed from indexed text), then the text is escaped and marked up.
    QUERY = {
        postgres: "SELECT s.content_type, s.item_id, s.title, s.path, s.published_at,
                ts_headline($1::regconfig, s.body, q,
                    'MaxFragments=2, MaxWords=24, MinWords=10, FragmentDelimiter= … , StartSel='
                    || chr(1) || ', StopSel=' || chr(2)) AS snippet,
                ts_rank(s.tsv, q)::float8 AS rank
            FROM engine_search s, websearch_to_tsquery($1::regconfig, $2) q
            WHERE s.tsv @@ q AND ($3::text IS NULL OR s.content_type = $3)
            ORDER BY rank DESC, s.published_at DESC NULLS LAST, s.id DESC
            LIMIT $4 OFFSET $5",
        sqlite: "SELECT s.content_type, s.item_id, s.title, s.path, s.published_at,
                snippet(engine_search_fts, 1, char(1), char(2), ' … ', 24) AS snippet,
                -bm25(engine_search_fts, 10.0, 1.0) AS rank
            FROM engine_search_fts JOIN engine_search s ON s.id = engine_search_fts.rowid
            WHERE engine_search_fts MATCH $2 AND ($3 IS NULL OR s.content_type = $3)
              AND ($1 IS NULL OR 1 = 1)
            ORDER BY rank DESC, s.published_at DESC NULLS LAST, s.id DESC
            LIMIT $4 OFFSET $5",
    };
    COUNT = {
        postgres: "SELECT count(*) FROM engine_search s, websearch_to_tsquery($1::regconfig, $2) q
            WHERE s.tsv @@ q AND ($3::text IS NULL OR s.content_type = $3)",
        sqlite: "SELECT count(*) FROM engine_search_fts JOIN engine_search s
                ON s.id = engine_search_fts.rowid
            WHERE engine_search_fts MATCH $2 AND ($3 IS NULL OR s.content_type = $3)
              AND ($1 IS NULL OR 1 = 1)",
    };
}
