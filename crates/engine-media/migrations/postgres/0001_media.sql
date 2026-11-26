-- Wrought `media` feature: uploaded files. The bytes live on disk under
-- `[media] dir` (public/<id>/ or private/<id>/); this table describes them.
CREATE TABLE engine_media (
    id bigserial PRIMARY KEY,
    -- Safe file stem (lowercase words joined by `-`) made from the uploaded
    -- name; the extension comes from the sniffed content, never the name.
    name text NOT NULL CHECK (name ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(name) <= 60),
    ext text NOT NULL CHECK (ext IN ('jpg', 'png', 'gif', 'webp', 'pdf')),
    mime text NOT NULL,
    original_name text NOT NULL DEFAULT '' CHECK (length(original_name) <= 255),
    bytes bigint NOT NULL,
    width integer,
    height integer,
    -- Widths of the resized copies written next to the original.
    variants jsonb NOT NULL DEFAULT '[]',
    sha256 text NOT NULL,
    alt text NOT NULL DEFAULT '' CHECK (length(alt) <= 500),
    -- Private files (form attachments) are never served at /media/.
    private boolean NOT NULL DEFAULT false,
    uploaded_by bigint,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX engine_media_list ON engine_media (private, id DESC);
