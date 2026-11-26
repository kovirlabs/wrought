## v0.6 "payments" (2026-11-26)

### `payments` is a feature of its own crate, PostgreSQL only, in no preset
- **Decision:** new crate `engine-payments`, engine-cli feature
  `payments = ["auth", "jobs", ...]` (the spec's dependencies) plus core's
  marker. `jobs` needs Postgres, so payments does too: `Payments::new`
  refuses SQLite with `engine_db::postgres_only("payments")`, and a SQLite
  site with `[payments]` is refused at start (build, dev and serve). Its
  statements are `{ postgres }` only; the catalogue test's second check
  accepts `engine_payments::` next to `engine_jobs::`. Not in `app`: a
  shop is a deliberate choice, and the fake provider and webhook routes
  should not appear in every app build. `check-features.sh` gains
  `payments`, `payments,content`, `payments,admin,db-sqlite` and
  `app,payments`.
- **Rejected:** putting it in `app` (every app binary would carry a
  checkout and a webhook endpoint); a SQLite path without jobs (applying
  events inline in the webhook request would lose the retry and lease
  guarantees the spec asks jobs for).

### Provider trait, Stripe Checkout Sessions, and a fake provider
- **Decision:** `Provider { name, create_checkout, parse_webhook, as_any }`;
  events are normalised to `paid | failed | expired | cancelled |
  refunded` with amount, currency and order/session/payment ids. The
  shared code (webhook endpoint, replay guard, job, state machine) never
  sees provider formats. **Stripe** (the Bloomery spec names no provider;
  Stripe is the common default) via Checkout Sessions, form-encoded with
  `price_data` lines from the engine's prices, `client_reference_id` +
  metadata (also on the PaymentIntent, for refunds), `Idempotency-Key:
  checkout-<order>`; keys only from `ENGINE_STRIPE_SECRET_KEY` /
  `ENGINE_STRIPE_WEBHOOK_SECRET`, `Debug` redacted, reqwest errors
  stripped of URLs. Tested against an in-process axum mock on an ephemeral
  127.0.0.1 port and recorded event fixtures; the API address cannot be
  configured at all (see "The secret key only goes to api.stripe.com").
- **Signatures:** one implementation of the Stripe scheme (`t=`, any of
  several `v1=` HMAC-SHA256 over `t.body`, constant-time, tolerance both
  ways, default 300 s); the fake provider uses the same function with its
  own header (`Wrought-Signature`).
- **Rejected:** a provider-agnostic crate such as `async-stripe` (a very
  large generated dependency for one endpoint and one webhook parser);
  PayPal or Mollie first (no spec preference; Stripe's scheme is the one
  most others copy).

### The secret key only goes to `api.stripe.com` (security review rule)
- **Decision:** a credential is never attached to a configurable URL
  without checks. `StripeConfig` has no address field; `Stripe::new` always
  uses `https://api.stripe.com`, and the URL is parsed and checked when the
  provider is built and again before each request: scheme `https`, host
  exactly `api.stripe.com` (so `api.stripe.com.evil.test` and
  `api.stripe.com@evil.test` fail), default port, no user name or
  password, no path, query or fragment. The mock-server override is a
  `#[cfg(test)]` constructor (`Stripe::with_mock`) that additionally
  accepts plain `http` to a loopback address only; neither `site.toml` nor
  any environment variable can reach it, so the adapter's mock tests moved
  from `tests/` into a unit-test module. The client carrying the key uses
  `redirect::Policy::none()`, and a 3xx answer is an error (the key is
  never re-sent elsewhere). The checkout URL Stripe returns, which the
  buyer is sent to, must be `https` (loopback `http` in tests).
- **Outbound callbacks:** payments makes none that carry a signature or
  secret. The fake provider signs its events and hands them to the webhook
  handling in process (no HTTP); success and cancel URLs carry no secret
  and are built from `ENGINE_PUBLIC_URL` / `[site] base_url`.
- **Tests:** refusals for a non-Stripe host, look-alike hosts, `http` to
  Stripe and to non-loopback hosts (also with the test opt-in), user info,
  other ports and paths; a 307 from the mock is not followed (the target
  records no request); an `http` checkout URL is refused.
- **Rejected:** an `ENGINE_STRIPE_API_BASE` variable for staging mocks (a
  deployment variable that redirects the live key is exactly what the rule
  forbids); following redirects only to the same host (no legitimate
  Stripe API call redirects).

### The fake provider is guarded by an environment variable
- **Decision:** `provider = "fake"` only starts with `ENGINE_PAYMENTS_FAKE=1`
  in the environment; `engine serve` then warns at start and its pages say
  "Test payments". Its routes (`/_engine/payments/fake/checkout/<id>/` and
  `/deliveries/<id>/`) exist only while it is the configured provider. Its
  events are signed and go through `Payments::receive`, the same function
  the HTTP webhook calls (in process, so no loopback HTTP or public URL is
  needed). Deliveries (raw body + header) are kept in memory and listed as
  JSON so a replay can be done by hand. Signing secret random per process
  (`ENGINE_PAYMENTS_FAKE_SECRET` to fix it).
- **Why:** a `site.toml` is committed and deployed; a setting in it alone
  could ship a shop that "accepts" every payment. Production environments
  do not set the variable, so a forgotten `fake` fails loudly at start.
- **Rejected:** guarding on `base_url` being https or non-localhost (staging
  sites are https too, and the check would be guesswork); debug builds
  only (tests and the example run release builds too); delivering the
  fake webhook over loopback HTTP (needs a reachable own address behind
  proxies; the in-process call exercises the same verification).

### Carts belong to accounts; checkout needs sign-in
- **Decision:** the cart is `engine_cart_lines(user_id, product, quantity)`
  (ids and quantities only, ≤ 50 lines, ≤ 99 each); shop posts from
  visitors who are not signed in redirect to sign-in. Orders belong to the
  account, which is what order history and the spec's `auth` dependency
  need.
- **Why:** no second anonymous cart cookie, no cart merging on sign-in, and
  CSRF is always the session's token. Product pages can stay published:
  anonymous visitors see "Sign in to buy"; signed-in visitors get every
  page per request (existing rule), so the add-to-cart form with their
  token appears without making shop pages `request` routes.
- **Rejected for now:** guest checkout (a cart cookie, an email field and
  order lookup by link): a later step if shops need it.

### Products: a content type when `content` is compiled in, else a file
- **Decision:** `[payments] product_type = "<type>"` makes a content type
  the catalogue (published items only; integer `price` in minor units,
  title field as name, optional `summary`, `image`, `image_alt`,
  `available`), which gives admin editing, revisions and published product
  pages for free. Without it, `products.toml` (`products_file`) is read at
  start. The catalogue is a trait in `engine-payments`; the content adapter
  lives in engine-cli, so the crate does not depend on `engine-content` and
  `payments` still needs only `auth` + `jobs`. Content listings record
  `c:<type>` / `i:<type>:<id>`, so published grids follow product edits.
  One currency per shop (`[payments] currency`).
- **Rejected:** a payments-owned products table with its own admin
  screens (duplicates content editing); requiring `content` (against the
  spec's dependency list); per-product currencies (multi-currency pricing
  is a pricing policy, not a field).

### Money is integer minor units end to end
- `i64` minor units in the catalogue, cart, orders (`bigint`), provider
  requests and events; checked multiplication and sums; formatting by ISO
  4217 exponent (JPY 0, GBP 2, KWD 3, a table of the exceptions) with
  symbols for GBP/USD/EUR/JPY and the code otherwise. Order lines copy
  name and price at checkout (`CHECK (amount = unit_amount * quantity)`).

### Webhooks: verify, store with a unique event id, queue in one transaction
- **Decision:** the endpoint verifies the signature on the raw body first
  (400, nothing stored, reason logged), then inserts the event with
  `ON CONFLICT (provider, event_id) DO NOTHING` and enqueues
  `payments.apply` with `Jobs::enqueue_in` in the same transaction. A
  duplicate is a 200 no-op. Genuine events of other types are acknowledged
  and not stored.
- **Why:** answering fast keeps providers from retrying; storing and
  queuing together means an accepted event is never lost or applied
  without a record; the unique id plus the timestamp tolerance is the
  replay guard.

### Order state machine applied by the job, with locks and version checks
- **Decision:** `pending → paid | failed | expired | cancelled | flagged`,
  `paid → refunded`; the same state again is a no-op; anything else is
  refused (event kept as `refused` with the reason, logged, order
  untouched). `paid` needs the event's amount and currency to equal the
  order's, otherwise the order becomes `flagged` (needs a person; never
  paid by mistake). Only full refunds change an order. The job locks the
  event row (`FOR UPDATE`), skips events no longer `received` (at-least-once
  delivery), and updates the order with `WHERE version = $read`; a
  conflict rolls back and the job retries. Paying clears the buyer's cart.
- **Rejected:** applying events inside the webhook request (a slow or
  failing database would make the provider retry into a half-applied
  state); `flagged` as a side flag on `pending` (a later correct event
  could then pay an order whose money was already wrong).

### Checkout idempotency
- The checkout form carries a fresh random key (`shop_cart().checkout_key`);
  `UNIQUE (user_id, idempotency_key)` makes a second post of the same form
  find the same order (and its checkout URL); the provider gets
  `checkout-<order id>` as its idempotency key. A provider error marks the
  pending order `failed` with the reason and sends the buyer back with a
  notice; nothing was charged.

### Commerce components
- `product_card`, `product_grid`, `cart`, `checkout_summary`,
  `order_history` in `engine-payments/site-templates/components/`, provided
  through `SiteExtension::template_dirs`, overridable by the site. One
  template each, arguments documented at the top of the file (ready for a
  manifest/versioning convention). Semantic HTML only (no classes beyond
  the existing `outline` button style, no colours): tables with captions
  and scoped headers, labelled quantity inputs, `aria-describedby` tying
  buttons to their product, `role="status"`/`role="alert"` regions for
  the post-redirect notices (`?shop=added` etc.), `<data value>` for
  amounts. Functions: `shop_products`, `shop_product`, `shop_cart`,
  `shop_orders`, `shop_order`, `money` (also a filter), global `shop`.
  Personal functions read `user` from the context, so shared fragments
  (which never get `user`) cannot show anyone's cart.

### Admin orders screen
- Read-only `/admin/orders/` (administrators): counts per state (flagged
  first when any), paging, and an order's lines, buyer, provider ids and
  events with their results. Behind engine-admin's new `payments` feature
  (`engine-admin?/payments`). Refunds are made at the provider; its webhook
  updates the order.

### Not done
- Guest checkout; partial refunds as a state; shipping, tax and discounts;
  stock levels; emails on payment (the `mail` hook is there: the job could
  send a receipt); other providers; S3-hosted product images (products use
  `image` paths/URLs).
