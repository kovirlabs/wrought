-- Related items and term listings look up items by term *and* type
-- (`term_id = ANY(...) AND content_type = ...`, grouped by item). One
-- index on all three answers them from the index alone, without visiting
-- the table; it replaces the index on term_id only.
CREATE INDEX engine_item_terms_term_type ON engine_item_terms (term_id, content_type, item_id);
DROP INDEX engine_item_terms_term;
