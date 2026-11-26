-- Room for bank debits and for records other than orders (see the spec's
-- "Payments and donations"):
--   * `processing`: a delayed payment method (ACH Direct Debit, SEPA) was
--     submitted and is waiting to settle; `returned`: a bank debit came back
--     after it was submitted or paid;
--   * an event that matches no order is kept as `unmatched` (not refused),
--     so a later record type (donations arrive by webhook with no order made
--     first) can claim it.
ALTER TABLE engine_orders DROP CONSTRAINT engine_orders_status_check;
ALTER TABLE engine_orders ADD CONSTRAINT engine_orders_status_check
    CHECK (status IN ('pending', 'processing', 'paid', 'failed', 'expired', 'cancelled',
                      'refunded', 'returned', 'flagged'));
ALTER TABLE engine_payment_events DROP CONSTRAINT engine_payment_events_kind_check;
ALTER TABLE engine_payment_events ADD CONSTRAINT engine_payment_events_kind_check
    CHECK (kind IN ('processing', 'paid', 'failed', 'expired', 'cancelled', 'refunded',
                    'returned'));
ALTER TABLE engine_payment_events DROP CONSTRAINT engine_payment_events_status_check;
ALTER TABLE engine_payment_events ADD CONSTRAINT engine_payment_events_status_check
    CHECK (status IN ('received', 'applied', 'flagged', 'refused', 'ignored', 'unmatched'));
