//! The `payments` feature's statements: PostgreSQL only (payments needs
//! `jobs`, which needs Postgres).

engine_db::statements! {
    CART_LINES = { postgres: "SELECT product, quantity FROM engine_cart_lines
            WHERE user_id = $1 ORDER BY added_at, product" };
    CART_ADD = { postgres: "INSERT INTO engine_cart_lines (user_id, product, quantity) VALUES ($1, $2, $3)
            ON CONFLICT (user_id, product)
            DO UPDATE SET quantity = least(engine_cart_lines.quantity + EXCLUDED.quantity, 99)" };
    CART_SET = { postgres: "INSERT INTO engine_cart_lines (user_id, product, quantity) VALUES ($1, $2, $3)
            ON CONFLICT (user_id, product) DO UPDATE SET quantity = EXCLUDED.quantity" };
    CART_REMOVE = { postgres: "DELETE FROM engine_cart_lines WHERE user_id = $1 AND product = $2" };
    CART_CLEAR = { postgres: "DELETE FROM engine_cart_lines WHERE user_id = $1" };
    CART_SIZE = { postgres: "SELECT count(*) FROM engine_cart_lines WHERE user_id = $1" };

    ORDER_INSERT = { postgres: "INSERT INTO engine_orders
                (public_id, user_id, currency, total, provider, idempotency_key)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (user_id, idempotency_key) DO NOTHING RETURNING id" };
    LINE_INSERT = { postgres: "INSERT INTO engine_order_lines
                (order_id, position, product, name, unit_amount, quantity, amount)
            VALUES ($1, $2, $3, $4, $5, $6, $7)" };
    ORDER_BY_KEY = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE user_id = $1 AND idempotency_key = $2" };
    ORDER_BY_ID = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE id = $1" };
    ORDER_BY_PUBLIC = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE public_id = $1" };
    ORDER_BY_SESSION = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE provider = $1 AND provider_session = $2" };
    ORDER_BY_PAYMENT = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE provider = $1 AND provider_payment = $2
            ORDER BY id DESC LIMIT 1" };
    ORDERS_OF_USER = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE user_id = $1 ORDER BY id DESC LIMIT $2" };
    ORDERS_PAGE = { postgres: "SELECT id, public_id, user_id, status, currency, total, provider,
                provider_session, provider_payment, checkout_url, note, version, created_at,
                updated_at, paid_at
            FROM engine_orders WHERE (CAST($1 AS TEXT) IS NULL OR status = $1)
            ORDER BY id DESC LIMIT $2 OFFSET $3" };
    ORDER_COUNTS = { postgres: "SELECT status, count(*) FROM engine_orders GROUP BY status ORDER BY status" };
    LINES_OF = { postgres: "SELECT order_id, position, product, name, unit_amount, quantity, amount
            FROM engine_order_lines WHERE order_id = ANY($1) ORDER BY order_id, position" };
    ORDER_SET_SESSION = { postgres: "UPDATE engine_orders SET provider_session = $2, checkout_url = $3,
                version = version + 1, updated_at = now()
            WHERE id = $1 AND status = 'pending'" };
    ORDER_TRANSITION = { postgres: "UPDATE engine_orders SET status = $3, note = $4,
                provider_payment = coalesce($5, provider_payment),
                paid_at = CASE WHEN $3 = 'paid' THEN now() ELSE paid_at END,
                version = version + 1, updated_at = now()
            WHERE id = $1 AND version = $2" };
    ORDER_NOTE = { postgres: "UPDATE engine_orders SET note = $3, version = version + 1, updated_at = now()
            WHERE id = $1 AND version = $2" };
    USER_EMAIL = { postgres: "SELECT email FROM engine_users WHERE id = $1" };

    EVENT_INSERT = { postgres: "INSERT INTO engine_payment_events
                (provider, event_id, kind, provider_type, order_ref, session, payment, amount,
                 currency, payload)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (provider, event_id) DO NOTHING RETURNING id" };
    EVENT_LOCK = { postgres: "SELECT id, provider, event_id, kind, provider_type, order_ref, session,
                payment, amount, currency, status, note, order_id, received_at
            FROM engine_payment_events WHERE id = $1 FOR UPDATE" };
    EVENT_DONE = { postgres: "UPDATE engine_payment_events SET status = $2, note = $3, order_id = $4,
                applied_at = now()
            WHERE id = $1" };
    EVENTS_OF_ORDER = { postgres: "SELECT id, provider, event_id, kind, provider_type, order_ref, session,
                payment, amount, currency, status, note, order_id, received_at
            FROM engine_payment_events WHERE order_id = $1 ORDER BY id" };
}
