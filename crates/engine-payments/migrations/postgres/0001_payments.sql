-- Wrought `payments` feature (PostgreSQL only, like `jobs`): carts, orders,
-- order lines and the provider events that move orders between states.

-- A signed-in visitor's cart: product ids and quantities only. Prices are
-- read from the catalogue when the cart is shown and again at checkout.
CREATE TABLE engine_cart_lines (
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    product text NOT NULL CHECK (product ~ '^[a-z0-9][a-z0-9_-]{0,79}$'),
    quantity integer NOT NULL CHECK (quantity BETWEEN 1 AND 99),
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, product)
);

CREATE TABLE engine_orders (
    id bigserial PRIMARY KEY,
    -- Shown to buyers and sent to the provider (`ord_...`).
    public_id text NOT NULL UNIQUE CHECK (public_id ~ '^ord_[A-Za-z0-9_-]{8,40}$'),
    user_id bigint REFERENCES engine_users (id) ON DELETE SET NULL,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'paid', 'failed', 'expired', 'cancelled', 'refunded', 'flagged')),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- Minor units (pence, cents, yen).
    total bigint NOT NULL CHECK (total >= 0),
    provider text NOT NULL,
    provider_session text,
    provider_payment text,
    checkout_url text,
    -- From the checkout form: a second post of the same form finds this
    -- order instead of making another.
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 16 AND 100),
    -- Why an order is flagged or what refused its last event.
    note text,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    paid_at timestamptz,
    UNIQUE (user_id, idempotency_key),
    UNIQUE (provider, provider_session)
);

CREATE INDEX engine_orders_user ON engine_orders (user_id, id DESC);
CREATE INDEX engine_orders_status ON engine_orders (status, id DESC);
CREATE INDEX engine_orders_payment ON engine_orders (provider, provider_payment);

-- Copied from the catalogue when the order is made, so later price or
-- name changes do not rewrite history.
CREATE TABLE engine_order_lines (
    order_id bigint NOT NULL REFERENCES engine_orders (id) ON DELETE CASCADE,
    position integer NOT NULL,
    product text NOT NULL,
    name text NOT NULL,
    unit_amount bigint NOT NULL CHECK (unit_amount >= 0),
    quantity integer NOT NULL CHECK (quantity BETWEEN 1 AND 99),
    amount bigint NOT NULL CHECK (amount = unit_amount * quantity),
    PRIMARY KEY (order_id, position)
);

-- Every verified webhook event the engine acts on. The unique provider
-- event id is the replay guard: a second delivery of the same event is a
-- no-op.
CREATE TABLE engine_payment_events (
    id bigserial PRIMARY KEY,
    provider text NOT NULL,
    event_id text NOT NULL CHECK (length(event_id) BETWEEN 1 AND 255),
    kind text NOT NULL CHECK (kind IN ('paid', 'failed', 'expired', 'cancelled', 'refunded')),
    provider_type text NOT NULL,
    order_ref text,
    session text,
    payment text,
    amount bigint,
    currency text,
    payload jsonb NOT NULL,
    -- received -> applied | flagged | refused | ignored
    status text NOT NULL DEFAULT 'received'
        CHECK (status IN ('received', 'applied', 'flagged', 'refused', 'ignored')),
    note text,
    order_id bigint REFERENCES engine_orders (id) ON DELETE SET NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    applied_at timestamptz,
    UNIQUE (provider, event_id)
);

CREATE INDEX engine_payment_events_order ON engine_payment_events (order_id, id);
