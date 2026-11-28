//! The `search` feature's statements (see `engine_db::sql`). `$1` is the
//! text search configuration.

engine_db::statements! {
    UPSERT = "INSERT INTO engine_search
                (content_type, item_id, title, body, path, published_at, updated_at, lang)
            VALUES ($2, $3, $4, $5, $6, $7, $8, $1::regconfig)
            ON CONFLICT (content_type, item_id) DO UPDATE SET
                title = excluded.title, body = excluded.body, path = excluded.path,
                published_at = excluded.published_at, updated_at = excluded.updated_at,
                lang = excluded.lang";
    // A whole reindex: every document in one statement (parallel
    // arrays), and a row is written only when it differs from what the
    // index holds, so rebuilding a current index writes nothing.
    BULK_UPSERT = "INSERT INTO engine_search
                (content_type, item_id, title, body, path, published_at, updated_at, lang)
            SELECT t, i, ti, b, NULLIF(p, ''), CAST(NULLIF(pa, '') AS TIMESTAMPTZ),
                   CAST($8 AS TIMESTAMPTZ), CAST($1 AS regconfig)
            FROM unnest(CAST($2 AS TEXT[]), CAST($3 AS BIGINT[]), CAST($4 AS TEXT[]),
                        CAST($5 AS TEXT[]), CAST($6 AS TEXT[]), CAST($7 AS TEXT[]))
                 AS x(t, i, ti, b, p, pa)
            ON CONFLICT (content_type, item_id) DO UPDATE SET
                title = excluded.title, body = excluded.body, path = excluded.path,
                published_at = excluded.published_at, updated_at = excluded.updated_at,
                lang = excluded.lang
            WHERE (engine_search.title, engine_search.body, engine_search.path,
                   engine_search.published_at, engine_search.lang)
                IS DISTINCT FROM (excluded.title, excluded.body, excluded.path,
                   excluded.published_at, excluded.lang)";
    // ...and the documents of items that are gone or no longer published.
    PRUNE = "DELETE FROM engine_search s WHERE NOT EXISTS (
                SELECT 1 FROM unnest(CAST($1 AS TEXT[]), CAST($2 AS BIGINT[])) AS x(t, i)
                WHERE x.t = s.content_type AND x.i = s.item_id)";
    ANY_DOCS = "SELECT EXISTS (SELECT 1 FROM engine_search)";
    REMOVE = "DELETE FROM engine_search WHERE content_type = $1 AND item_id = $2";
    COUNT_DOCS = "SELECT count(*) FROM engine_search";
    // Postgres, one round trip per page of results: the matches ranked,
    // their number (a window count over the same scan: ranking already
    // visits every match, so the total costs no second scan), and for the
    // page's rows only their text and the query's lexemes, from which the
    // engine cuts the snippets (`snippet.rs`; `ts_headline` parses and
    // stems each document again, ten times the cost of the rest). The
    // ranking carries narrow rows (id, rank) through the window and the
    // sort; the text is read for the page's rows only, by primary key.
    PAGE = "SELECT s.content_type, s.item_id, s.title, s.path, s.published_at,
                s.body AS snippet, r.rank, r.total, r.lexemes
            FROM (SELECT s.id, s.published_at, ts_rank(s.tsv, q)::float8 AS rank,
                    count(*) OVER () AS total, q::text AS lexemes
                FROM engine_search s, websearch_to_tsquery($1::regconfig, $2) q
                WHERE s.tsv @@ q AND ($3::text IS NULL OR s.content_type = $3)
                ORDER BY rank DESC, s.published_at DESC NULLS LAST, s.id DESC
                LIMIT $4 OFFSET $5) r
            JOIN engine_search s ON s.id = r.id
            ORDER BY r.rank DESC, r.published_at DESC NULLS LAST, r.id DESC";
    // The same with `ts_headline`, for text search configurations the
    // engine has no stemmer for (only for the page's rows, as above).
    PAGE_HEADLINE = "SELECT s.content_type, s.item_id, s.title, s.path, s.published_at,
                ts_headline($1::regconfig, s.body, websearch_to_tsquery($1::regconfig, $2),
                    'MaxFragments=2, MaxWords=24, MinWords=10, FragmentDelimiter= … , StartSel='
                    || chr(1) || ', StopSel=' || chr(2)) AS snippet,
                r.rank, r.total, ''::text AS lexemes
            FROM (SELECT s.id, s.published_at, ts_rank(s.tsv, q)::float8 AS rank,
                    count(*) OVER () AS total
                FROM engine_search s, websearch_to_tsquery($1::regconfig, $2) q
                WHERE s.tsv @@ q AND ($3::text IS NULL OR s.content_type = $3)
                ORDER BY rank DESC, s.published_at DESC NULLS LAST, s.id DESC
                LIMIT $4 OFFSET $5) r
            JOIN engine_search s ON s.id = r.id
            ORDER BY r.rank DESC, r.published_at DESC NULLS LAST, r.id DESC";
    COUNT = "SELECT count(*) FROM engine_search s, websearch_to_tsquery($1::regconfig, $2) q
            WHERE s.tsv @@ q AND ($3::text IS NULL OR s.content_type = $3)";
}
