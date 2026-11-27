-- Keep each document's tsvector in its row (compressed) instead of in the
-- TOAST table: ranking reads the tsvector of every match, and fetching it
-- out of line cost about three times the ranking itself (201 articles:
-- 1.6 ms against 0.5 ms per search). The text stays out of line; only the
-- page's rows read it. New storage applies to rows written from now on, so
-- the index is emptied here and refilled by the engine (`engine build`, and
-- `engine serve` at start and every minute, as after a crash).
ALTER TABLE engine_search ALTER COLUMN tsv SET STORAGE MAIN;
TRUNCATE engine_search;
