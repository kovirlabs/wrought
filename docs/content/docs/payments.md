+++
title = "Payments and Commerce components"
description = "Checkout through Stripe (or a fake provider for tests), signed webhooks, orders with a state machine, a server-side cart, and the product card, product grid, cart, checkout summary and order history components (PostgreSQL only)."
weight = 20
+++

The `payments` feature sells things: a cart for each signed-in visitor,
checkout through a payment provider's hosted page, signed webhooks that
confirm the payment, orders that move through a fixed set of states, and
the Commerce components that show all of it. It needs `auth` and `jobs`
(the spec's dependencies), and because `jobs` needs PostgreSQL, so does
`payments`: a SQLite site with `[payments]` is refused at start.

`payments` is in no preset; build it in on purpose:

```sh
cargo build --release --features app,payments
```

Without the feature there are no shop routes and the Commerce components
do not exist.

## Setup

```toml
[payments]
provider = "stripe"          # or "fake" (tests and local development)
currency = "GBP"             # ISO 4217; one currency per shop
product_type = "product"     # products from this content type ...
# products_file = "products.toml"   # ... or from a file (the default)
success_path = "/shop/orders/"      # buyers come back here (?order=<id>)
cancel_path = "/shop/cart/"         # and here when they give up
webhook_tolerance_seconds = 300
```

No key ever goes in `site.toml`. Stripe's come from the environment:

| Variable | What |
| --- | --- |
| `ENGINE_STRIPE_SECRET_KEY` | the secret API key (`sk_live_...` / `sk_test_...`) |
| `ENGINE_STRIPE_WEBHOOK_SECRET` | the webhook endpoint's signing secret (`whsec_...`) |
| `ENGINE_PAYMENTS_FAKE=1` | allows `provider = "fake"` (refused without it) |
| `ENGINE_PAYMENTS_FAKE_SECRET` | optional fixed signing secret for the fake provider |
| `ENGINE_PUBLIC_URL` | the site's public address, for the provider's return links |

Keys are redacted in `Debug` output and errors, and never logged. In the
Stripe dashboard, point a webhook endpoint at
`https://<your site>/_engine/payments/webhook/stripe/` with the events
`checkout.session.completed`, `checkout.session.async_payment_succeeded`,
`checkout.session.async_payment_failed`, `checkout.session.expired` and
`charge.refunded`. `engine serve` prints the webhook address when it
starts.

Payment events are applied by [job workers](/docs/jobs-and-mail/):
`engine serve` runs them, or `engine jobs work` when the server runs with
`--no-workers`.

### Products

Prices are always read from the catalogue on the server; a price a browser
sends is ignored. With `product_type`, products are a content type, edited
in the admin like any other content, and only published items are for
sale. The type needs an integer field `price` (the price of one in minor
units: pence, cents, yen); the name is the type's title field, and the
fields `summary`, `image`, `image_alt` and `available` (a boolean; unticked
means shown but not sold) are used when they exist:

```toml
# types/product.toml
label = "Product"

[[fields]]
name = "title"
type = "text"
required = true

[[fields]]
name = "price"
type = "integer"
required = true
min = 0

[[fields]]
name = "available"
type = "boolean"
```

Without `content` (or without `product_type`) products come from a TOML file,
`products.toml` by default, read when the server starts:

```toml
[[products]]
id = "mug"                 # what the cart stores
name = "Enamel mug"
price = 1200               # £12.00
summary = "Speckled blue enamel, 350 ml."
url = "/shop/mug/"         # optional page
image = "/img/mug.jpg"     # optional, with image_alt
available = true
```

### Money

Amounts are integers in the currency's minor unit everywhere: the
catalogue, the cart, orders, the provider and webhooks. They are formatted
by the currency's ISO 4217 exponent: `£12.50`, `¥1,500` (JPY has no minor
unit), `KWD 1.234` (three decimals). The `money` filter and function do the
same in templates: `{{ 1250 | money }}`, `{{ money(1500, "JPY") }}`.

## How a purchase goes

1. A signed-in visitor adds products to their cart
   (`POST /_engine/shop/cart/`). The cart lives in the database and holds
   product ids and quantities only (at most 50 products, 99 of each). Carts
   belong to accounts: visitors who are not signed in are sent to sign in.
2. The checkout form (`POST /_engine/shop/checkout/`) prices the cart again
   from the catalogue, stores a `pending` order with its lines, asks the
   provider for a hosted checkout and sends the visitor there. The form
   carries an idempotency key: posting it twice finds the same order and
   the same checkout. The provider gets a key derived from the order too, so
   a retried request makes no second session.
3. The provider sends a webhook. The engine checks the signature over the
   raw body, stores the event under the provider's event id and queues a
   `payments.apply` job in the same transaction, and answers 200.
4. The job applies the event to the order (below). Paying empties the
   visitor's cart.

Stripe sessions carry the order's id as `client_reference_id` and in
metadata, also on the PaymentIntent, so refunds find their order.

## Webhooks

`POST /_engine/payments/webhook/<provider>/` (the configured provider's
name; any other is 404):

| Delivery | Answer | Stored |
| --- | --- | --- |
| signed, new event | 200 `ok` | the event, and a job to apply it |
| the same event again (a replay or a provider retry) | 200 `ok` | nothing |
| a genuine event of a type the engine does not use | 200 `ok` | nothing |
| bad or missing signature, changed body, timestamp outside the tolerance | 400 `refused` | nothing |
| the database failed | 500 | nothing (the provider retries) |

The signature is the scheme Stripe uses: a header
`t=<unix time>,v1=<hex HMAC-SHA256 of "<t>.<raw body>">`; several `v1`
entries may be present (any may match), the comparison is constant-time,
and the timestamp must be within `webhook_tolerance_seconds` (both ways).
Replays are stopped twice: an old capture fails the timestamp check, and a
recent one hits the unique (provider, event id) and does nothing. Every
refusal is logged with its reason. Bodies larger than 256 KB are refused.

## Orders

| From | Event | To |
| --- | --- | --- |
| pending | paid, amount and currency equal to the order's | paid |
| pending | paid, any other amount or currency | flagged |
| pending | failed / expired / cancelled | failed / expired / cancelled |
| paid | refunded in full | refunded |
| any | the same as its state (paid when paid) | nothing (a no-op) |
| anything else | | refused: the order stays, the event is kept with the reason, and it is logged |

*Flagged* orders need a person: the money received does not match the
order, so it is neither paid nor forgotten. Partial refunds do not change
an order. Each change runs in one transaction that locks the event row and
updates the order only if its `version` is still the one read (a conflict
rolls back and the job retries), so a job delivered twice applies its event
once.

Administrators see orders under **Orders** in the admin
(`/admin/orders/`): counts per state, flagged ones first, and each order's
lines, buyer, provider ids and the events that moved it, with the reason
for every refusal. The screen is read-only; refunds are made at the
provider (its `charge.refunded` webhook then marks the order).

Tables: `engine_cart_lines`, `engine_orders`, `engine_order_lines` (names
and prices copied at checkout, so later catalogue edits do not rewrite
history), `engine_payment_events`.

## Providers

The provider is a Rust trait (`engine_payments::Provider`): create a
checkout (lines in minor units, currency, success and cancel URLs, an
idempotency key) and verify and parse a webhook into a normalised event
(`paid`, `failed`, `expired`, `cancelled`, `refunded`, with amount,
currency and the order, session and payment ids). The webhook endpoint,
replay protection, job and state machine are shared, so another provider
is one adapter.

**Stripe** uses Checkout Sessions over HTTPS (`reqwest` with rustls). The
secret key is only ever sent over `https` to exactly `api.stripe.com`: the
address is fixed in the engine (there is no setting for it), checked before
every request (scheme, exact host, no user name or password, default
port), and the client never follows a redirect. The checkout address
Stripe returns must be `https`. The adapter's tests run against an
in-process mock of the API (through a test-only constructor that allows
loopback `http`) and recorded event payloads; they make no network calls.

**The fake provider** is for tests and local development. Its checkout is
a page the engine serves (`/_engine/payments/fake/checkout/<session>/`),
marked as a test, where you choose what happens: pay, decline, cancel, let
it expire, pay one minor unit too little (to see a flagged order), and
refund once paid. Each choice produces a *signed* event (header
`Wrought-Signature`, same scheme) that goes through exactly the same
verification, storage and job as a real webhook. Its deliveries (raw body
and signature) are listed as JSON at
`/_engine/payments/fake/deliveries/<session>/`, so you can replay one with
curl and watch it change nothing:

```sh
D=$(curl -s http://localhost:4400/_engine/payments/fake/deliveries/cs_fake_.../)
curl -i -H "Wrought-Signature: $(echo "$D" | jq -r '.deliveries[0].signature')" \
  --data-binary "$(echo "$D" | jq -r '.deliveries[0].body')" \
  http://localhost:4400/_engine/payments/webhook/fake/
```

It never runs by accident: `provider = "fake"` is refused unless the
environment has `ENGINE_PAYMENTS_FAKE=1`, so a `site.toml` that still names
it cannot go live; when it runs, `engine serve` warns at start and its
pages say no money moves. Its routes exist only while it is the configured
provider.

## Security

- Prices and totals are computed on the server from the catalogue at
  checkout; the provider is told those amounts, and the paid event's amount
  and currency must equal the order's or the order is flagged.
- Every shop form carries the session's CSRF token and is refused from
  other origins; the fake checkout page uses the signed-out token cookie.
- Webhooks are verified on the raw body before anything is parsed or
  stored; forged, changed or stale deliveries change nothing.
- Orders and order history are visible only to their account (and to
  administrators in the admin); order ids are random (`ord_` + 24
  characters).
- Keys only from the environment, redacted everywhere.

## Commerce components

As with every component, a file of the same name in the site's
`templates/components/` replaces the built-in one. All are semantic HTML,
work without JavaScript (forms post and redirect back), take their look
from TagBrush, and use tables with captions and header cells, labelled
inputs and `role="status"` / `role="alert"` regions for notices. Forms
need a signed-in visitor on a page rendered per request; on published pages
the components show sign-in links instead. Signed-in visitors get every
page per request anyway, so product pages can stay published.

| Component | Shows | Arguments |
| --- | --- | --- |
| `product_card` | name (linked), summary, image, price, and an add-to-cart form with a quantity | `product` (required), `level` (3), `buy` (false hides the form), `next` |
| `product_grid` | a list of product cards, and a notice after adding | `products` (default `shop_products()`), `limit`, `title`, `level` (2), `next` |
| `cart` | the visitor's cart as a table; change a quantity (0 removes) or remove a line | `cart` (default `shop_cart()`), `title`, `level`, `next` |
| `checkout_summary` | what will be paid and the pay button (with the idempotency key) | `cart`, `title`, `level`, `label`, `next` |
| `order_history` | the visitor's orders with lines, totals and states; reports on `?order=` after paying | `orders` (default `shop_orders()`), `limit`, `title`, `level`, `order` |

```jinja
{% set c = shop_cart() %}
{{ component("cart", cart=c) }}
{% if not c.empty %}{{ component("checkout_summary", cart=c) }}{% endif %}
```

Template functions behind them: `shop_products(limit=None)`,
`shop_product(id)`, `shop_cart()`, `shop_orders(limit=20)`,
`shop_order(id)` (the visitor's own orders only), `money(amount,
currency=None)`, and the global `shop` (`currency`, `cart_action`,
`checkout_action`, `cart_page`, `orders_page`). Product listings from a
content type record its cache tags, so published pages with a product grid
are re-rendered when a product changes.

## Example

`examples/shop` is a small shop on the fake provider: products from a
content type, published product pages, a cart and checkout for signed-in
visitors, and the order history. See its `README.md`.
