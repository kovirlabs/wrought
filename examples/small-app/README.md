# Small App example

The `app` preset's example, built only from Wrought's components:

- **Accounts:** registration (`/register/`, open), sign-in (`/login/`),
  password reset by email (`/reset/`), a profile form and an account menu,
  all in the site's own layout.
- **A form with an emailed receipt:** `/support/` (`forms/support.toml`) is
  validated on the server, guarded against spam (honeypot, time trap, rate
  limit), stored, and answered with a receipt to the sender and a notice to
  the team, both sent through the job queue.
- **A multi-step form:** `/onboarding/`.
- **An account page** (`/me/`, signed-in only) with a stat card, a chart, a
  data table (sort, filter, pages without JavaScript), an alert, a modal, a
  toast and an empty state, over the visitor's own submissions.
- **The fragment cache:** `/status/` has a shared fragment (latest updates,
  dropped when an update changes) and a per-user one (your requests).
- **Updates** published from the database (`types/update.toml`).

It needs an engine built with `--features app`, PostgreSQL and, for mail, an
SMTP server. From the repository root:

```sh
cargo build --release --features app
docker compose -f deploy/compose.postgres.yml up -d --wait
docker compose -f deploy/compose.mailpit.yml up -d --wait
export ENGINE_DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought
export ENGINE_SMTP_URL=smtp://127.0.0.1:51025       # Mailpit: delivers nothing
export ENGINE_SECRET_KEY="$(openssl rand -base64 48)"  # two-factor secrets
E=./target/release/engine
S=examples/small-app

$E migrate --site $S
$E content seed --site $S
$E build --site $S
$E user create --site $S --email you@example.org --name You   # an administrator
$E serve --site $S
```

Open <http://localhost:4400/register/>, create an account, send a request
at `/support/`, and read the receipt at <http://127.0.0.1:58025/>. The
administrator sees the submissions under **Forms** in `/admin/`.

New accounts confirm their address first: the welcome message (in
Mailpit) has the link. Every account can turn on two-factor sign-in at
`/account/profile/`. A support request can carry a screenshot: it is checked
by its content, stripped of metadata and stored privately; the team
downloads it from the submission in the admin.

`engine jobs list --site $S` (or **Jobs** in the admin) shows the mail jobs;
`curl -sI http://localhost:4400/status/` shows `X-Wrought-Fragments`.

### The JSON API

Updates can also be written by a program. Make a token for your account and
use it:

```sh
TOKEN=$($E api token create --site $S --email you@example.org --name script \
          --scope read --scope write:update)
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:4400/api/v1/content/update/
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"fields": {"title": "Maintenance tonight", "body": "From 22:00, about an hour."}, "status": "published"}' \
     http://localhost:4400/api/v1/content/update/
```

The new update is published like an edit in the admin: `/status/` shows it
at once. Changes need the item's version (`If-Match`); see the API docs
page.

The acceptance test `crates/engine-cli/tests/app.rs` runs all of this
(registration, login, the form, the receipt read back from Mailpit, the
spam guard, the fragment cache, a password reset, workflow review) against
a throwaway database: `scripts/test-postgres.sh -p engine-cli --test app`.
