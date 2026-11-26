//! The `media` feature's statements (see `engine_db::sql`).

engine_db::statements! {
    INSERT = "INSERT INTO engine_media
            (name, ext, mime, original_name, bytes, width, height, variants, sha256, alt,
             private, uploaded_by, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)
        RETURNING id";
    GET = "SELECT id, name, ext, mime, original_name, bytes, width, height, variants, sha256,
            alt, private, uploaded_by, version, created_at, updated_at
        FROM engine_media WHERE id = $1";
    LIST = "SELECT id, name, ext, mime, original_name, bytes, width, height, variants, sha256,
            alt, private, uploaded_by, version, created_at, updated_at
        FROM engine_media WHERE private = $1 AND (CAST($2 AS TEXT) IS NULL OR mime LIKE $2)
        ORDER BY id DESC LIMIT $3 OFFSET $4";
    COUNT = "SELECT count(*) FROM engine_media WHERE private = $1 AND (CAST($2 AS TEXT) IS NULL OR mime LIKE $2)";
    SET_ALT = "UPDATE engine_media SET alt = $3, version = version + 1, updated_at = $4
        WHERE id = $1 AND version = $2 RETURNING version";
    VERSION = "SELECT version FROM engine_media WHERE id = $1";
    DELETE = "DELETE FROM engine_media WHERE id = $1 AND version = $2";
}
