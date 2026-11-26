# Shop example

The `payments` feature's example, built from the Commerce components:

- **Products** are a content type (`types/product.toml`), edited in the
  admin and published as pages (`/shop/`, `/shop/<slug>/`).
- **A cart** for each signed-in visitor (`/shop/cart/`), with the checkout
  button.
- **Checkout** through the *fake* payment provider: a test checkout page
  served by the engine, where you pay, decline, cancel, let it expire, pay
  too little (the order is flagged) or refund. Each choice sends a signed
  webhook through the same checks as a real provider's.
- **Order history** (`/shop/orders/`), and **Orders** in the admin.

It needs an engine built with `--features app,payments` and PostgreSQL.
From the repository root:

```sh
cargo build --release --features app,payments
docker compose -f deploy/compose.postgres.yml up -d --wait
export ENGINE_DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought
export ENGINE_PAYMENTS_FAKE=1          # allows the fake provider
E=./target/release/engine
S=examples/shop

$E migrate --site $S
$E content seed --site $S
$E build --site $S
$E user create --site $S --email you@example.org --name You   # an administrator
$E serve --site $S
```

Open <http://localhost:4400/register/> (or sign in as the administrator),
add things to the cart from <http://localhost:4400/shop/>, and check out.

Without `ENGINE_PAYMENTS_FAKE=1` the engine refuses to start: the fake
provider takes no money. For a real shop set `provider = "stripe"` in
`site.toml` and `ENGINE_STRIPE_SECRET_KEY` and
`ENGINE_STRIPE_WEBHOOK_SECRET` in the environment (see
`docs/content/docs/payments.md`).
