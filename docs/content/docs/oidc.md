+++
title = "Single sign-on (OpenID Connect)"
description = "Sign in with an OpenID Connect provider (Keycloak, Authentik, Google, Microsoft, Dex...): setup, settings, linking accounts and the security rules."
weight = 20
+++

The `oidc` feature lets visitors sign in with an account they have at an
OpenID Connect provider: a company's Keycloak, Authentik or Microsoft Entra,
Google, GitLab, Dex and others. It builds on accounts (`auth`) and is in no
preset, so build the engine with it:

```sh
cargo build --release -p engine-cli --features cms,oidc   # or app,oidc
```

A provider adds a **Sign in with ...** button to `/account/login/` and to
the `login_form` component, and a *Sign-in providers* section to
`/account/profile/` where people link and unlink providers. Without the
feature there are no `/account/oidc/` routes, and `[[auth.oidc]]` in
`site.toml` is refused with a message saying which feature is missing.

## Setting up a provider

1. Register a client (an "application") at the provider:
   * type: web application, *authorization code* flow;
   * redirect URI: `https://example.org/account/oidc/<name>/callback/`,
     with your site's public address (`[auth] public_url`, else
     `ENGINE_PUBLIC_URL`, else `[site] base_url`) and the provider's
     `name` from `site.toml`;
   * scopes `openid email profile`;
   * PKCE with S256 is always used; enable it if the provider asks.
2. Add it to `site.toml`:

   ```toml
   [[auth.oidc]]
   name = "company"                  # a-z, 0-9, _: in URLs and the variable below
   label = "Company account"         # the button says "Sign in with Company account"
   issuer = "https://id.example.org/realms/staff"
   client_id = "wrought"
   ```

3. Give the engine the client secret in the environment, never in
   `site.toml`:

   ```sh
   export ENGINE_OIDC_COMPANY_CLIENT_SECRET='...'
   ```

   `engine serve` refuses to start when a provider's secret is missing.
   The secret is not printed in logs or error messages.

The engine reads the provider's settings from
`<issuer>/.well-known/openid-configuration` the first time someone uses it
(so the site starts even when the provider is down) and keeps them for an
hour.

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | (required) | Short name: `/account/oidc/<name>/...` and `ENGINE_OIDC_<NAME>_CLIENT_SECRET`. |
| `label` | (required) | Shown on buttons and messages. |
| `issuer` | (required) | The issuer exactly as the provider's discovery document states it, including any trailing slash. |
| `client_id` | (required) | The client's id at the provider. |
| `scopes` | `["openid", "email", "profile"]` | Scopes asked for; `openid` is always sent. |
| `public` | `false` | A public client: PKCE only, no client secret (then the variable must not be set). |
| `create_accounts` | `registration = "open"` | Make an account for someone who signs in with the provider and has none here. Needs `roles`. |
| `allowed_hosts` | `[]` | Other hosts (names only) the provider's endpoints may be on; by default only the issuer's host. |

Several providers can be listed (each its own `[[auth.oidc]]`), but not two
with the same issuer.

## Who gets in

* **Someone whose provider identity is linked** to an account signs in to
  that account. A new session is made, as after a password sign-in; if the
  account has two-factor sign-in on, the code is still asked for (the
  provider replaces the password, not the second factor). Blocked
  accounts are refused. Failed attempts count towards the same per-address
  limit as failed passwords (`max_failures_per_address`).
* **Someone with no linked identity** gets a new account only when
  `create_accounts` is on and the provider says their email address is
  verified (`email_verified`) and no account here uses that address. The
  account gets `[auth] default_roles` (never `admin`), no password, and
  counts towards `max_registrations_per_address`. It receives the
  `welcome` message.
* **An identity whose email address belongs to an existing account is never
  signed in to it**, even when the provider vouches for the address. The
  visitor is told to sign in with that account's password and link the
  provider on the profile page. Accounts are matched to identities by the
  provider's issuer and subject (its permanent user id), never by email
  address: providers differ in how well they check addresses, an address can
  change hands, and a provider anyone can register at could otherwise be
  used to enter someone else's account.

## Linking and unlinking

On `/account/profile/`, *Link* starts a sign-in at the provider for the
account that is signed in; the identity is linked when it comes back, unless
it is already linked to another account or the account already has another
identity from that provider. *Unlink* removes it, except when the account
has no password and this is its last provider: it would have no way to sign
in. Such an account can set a password with *Forgot your password?* on the
sign-in page (the reset link goes to its verified address), and then unlink.

The admin's account page lists an account's linked providers.

## Security details

* The button is a form posted with the sign-in page's CSRF token, so another
  site cannot sign a visitor in to an account of its choosing.
* Each attempt has a random `state`, `nonce` and PKCE verifier, kept in the
  database under the SHA-256 of a random token in a `__Host-wrought_oidc`
  cookie (HttpOnly, Secure, SameSite=Lax, ten minutes). When the browser
  comes back the row is deleted as it is read, the `state` is compared in
  constant time, and the `iss` parameter (if the provider sends one) must be
  the issuer: an answer works once, only in the browser that started it.
* The ID token's signature is checked with the provider's published keys
  (RS256/384/512, PS256/384/512, ES256, ES384; `none` and HMAC are
  refused, and the algorithm must fit the key). Keys are cached for an
  hour; a token signed with an unknown key makes the engine fetch them
  again, at most once a minute. `iss` must equal the issuer, `aud` contain
  the client id (and `azp` be the client id when there are several
  audiences), `exp`, `iat` and `nbf` hold within a minute's clock
  difference, and `nonce` match.
* The issuer and every endpoint must be `https://`. Plain `http://` is only
  accepted for this machine, `localhost`, `127.0.0.0/8` and `::1` (a
  provider for development). URLs with a user name or password are
  refused. This is checked when `site.toml` is loaded and again before
  every request, and the client secret is only ever sent to a token
  endpoint that passes it. The discovery document's `issuer` must equal
  the configured one exactly.
* The authorization, token and key endpoints must be on the issuer's host
  (any port). A provider that serves one of them from another host must
  name it: `allowed_hosts = ["login.example.org"]`.
* The engine's sign-in and profile pages allow forms to lead to the
  providers' origins in their `Content-Security-Policy` (`form-action`),
  because the button's answer is a redirect to the provider. If your own
  pages set a CSP and show the `login_form` component, add the issuer's
  origin to their `form-action`. A provider whose sign-in page is on
  another origin than its issuer gets a *Continue* link instead of a
  redirect.
* Provider answers are limited in size and time (10 s); redirects are
  never followed, and a redirect from the provider is an error.

## Trying it locally with Dex

`deploy/compose.oidc.yml` runs [Dex](https://dexidp.io/) on
`127.0.0.1:59110` with three users (`alice@`, `bob@`,
`carol@example.org`, password `password`) and a client `wrought-test`
(secret `wrought-test-secret`) whose redirect URIs are engines on
`http://localhost:31111` to `31119` with a provider named `dex`. These are
public test values: never use them anywhere else.

```sh
docker compose -f deploy/compose.oidc.yml up -d --wait
```

```toml
[[auth.oidc]]
name = "dex"
label = "Dex"
issuer = "http://127.0.0.1:59110/dex"
client_id = "wrought-test"
```

```sh
ENGINE_OIDC_DEX_CLIENT_SECRET=wrought-test-secret \
  engine serve --site mysite --port 31113
```

Open `http://localhost:31113/account/login/` and use *Sign in with Dex*.
`scripts/test-postgres.sh` starts the same container for the end-to-end test (`WROUGHT_NO_OIDC=1` skips it); stop it
with `docker compose -f deploy/compose.oidc.yml down -v`.

## Limits

* The email address and name are read from the ID token; providers that only
  put them in the UserInfo response need to be configured to include them
  in the token (most can).
* Only the authorization code flow, with the answer in the query string.
  No front- or back-channel logout: signing out of the site does not sign
  out of the provider, and the reverse.
* Clients authenticate with `client_secret_basic` or `client_secret_post`
  (or none, for public clients); `private_key_jwt` is not supported.
