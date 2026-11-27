-- The search index is derived from the content and rebuilt whenever it is
-- found empty (`engine build`, `engine serve` at start and every minute), so
-- it is kept out of the write-ahead log: writing it never waits for a disk
-- sync, and it adds nothing to WAL archives or replication. After a crash
-- or a failover PostgreSQL empties unlogged tables; the engine refills it.
ALTER TABLE engine_search SET UNLOGGED;
