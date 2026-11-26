## v0.6 "oidc": single sign-on with OpenID Connect

### `oidc` is a module of engine-auth behind a feature, not a crate
- **Decision:** `engine-auth/src/oidc/` with an `oidc` feature of engine-auth
  (optional `reqwest` and `ring`), switched on by engine-cli's `oidc` feature
  (`auth` + `engine-auth/oidc` + core's `oidc` marker). The settings
  (`ProviderConfig`, `[[auth.oidc]]` in `AuthConfig`) and `Identity` /
  `Auth::identities` are always compiled, without dependencies: a build
  without the feature refuses `[[auth.oidc]]` with a message naming the
  feature, and the admin lists identities without a feature of its own
  (empty list without `oidc`). Routes exist only with the feature *and* at
  least one provider. Not in any preset: it adds an HTTP client stack and
  is only useful with a provider.
- **Why:** sign-in by provider has to reach into the login page, the profile
  page, sessions, rate limits, registration rules and the Accounts
  components, all inside engine-auth; a separate crate would need hooks into
  each.
- **Rejected:** `engine-oidc` crate (hooks for every page and private
  account functions); a top-level `[oidc]` section (would need a new field
  in core's `SiteConfig`; providers belong with the other `[auth]` rules).

### Settings in `[[auth.oidc]]`, secret only from `ENGINE_OIDC_<NAME>_CLIENT_SECRET`
- `name` (`[a-z][a-z0-9_]*`, max 32: URLs and the variable), `label`,
  `issuer`, `client_id`, `scopes` (default `openid email profile`; `openid`
  always sent), `public` (PKCE only), `create_accounts`. Two providers may
  not share a name or an issuer (identities are keyed by issuer).
- A confidential provider without its secret, or a public one with a secret
  set, makes `engine serve` refuse to start (not `build`/`check`, which
  never talk to providers). The secret has a redacting `Debug`; provider
  errors never carry codes, tokens or secrets.

### Authorization code + PKCE (S256), state and nonce in a database row keyed by a cookie
- **Decision:** start is `POST /account/oidc/<name>/start/` with the
  signed-out double-submit CSRF token (no login CSRF via GET). It stores
  `state`, `nonce`, PKCE verifier, purpose (`login`/`link` with the account
  id) and `next` in `engine_oidc_states` under the SHA-256 of a random token
  set as `__Host-wrought_oidc` (HttpOnly, Secure, **SameSite=Lax** so it
  comes back on the provider's cross-site redirect, 10 minutes). The
  callback deletes the row as it reads it (`DELETE ... RETURNING`, single
  use even under races, expired rows ignored), compares `state` in constant
  time, checks the provider in the path (per-provider redirect URIs against
  mix-up) and an RFC 9207 `iss` parameter when present. Failures count
  towards the per-address sign-in limit.
- **Rejected:** state in a signed or encrypted cookie only (cannot be made
  single use); one shared callback URL (mix-up between providers);
  `form_post` response mode (needs a cross-site POST, which the Lax cookie
  would not accompany).

### Redirect to the provider, and `form-action`
- The start POST answers 303 to the provider. Browsers check redirects after
  a form submission against the submitting page's CSP `form-action`, and
  engine pages say `form-action 'self'`; so the login and profile pages add
  the configured issuers' origins to `form-action`. If a provider's
  authorization endpoint is on another origin than its issuer, the start
  answers with a page and a *Continue* link instead (rare; no CSP change
  needed). Site pages using the `login_form` component get no CSP from the
  engine; the docs tell sites with their own CSP to add the issuer origin.
- **Rejected:** always an interstitial page (an extra click/flash for every
  provider); `form-action *` (gives up the protection for every form).

### Discovery and keys: lazy, cached, refresh on unknown `kid` at most once a minute
- Discovery is fetched on first use (so the site starts when the provider is
  down) and cached an hour; its `issuer` must equal the configured one
  byte for byte; every endpoint must be https, plain http only for
  127.0.0.1, ::1 and localhost (for the local test provider). A provider
  that lists PKCE methods without S256 is refused. The JWKS is cached an
  hour; a token whose key is not in the set triggers one refetch, at most
  once per minute (counted even when the fetch fails). Responses are capped
  at 512 KiB and 10 s, redirects never followed (`reqwest`, workspace
  dependency, rustls with ring).
- Token endpoint authentication: `client_secret_basic` (default when the
  provider does not say), else `client_secret_post`; public clients send
  only `client_id`. `private_key_jwt` not supported.

### ID token checks with `ring`, no JOSE crate
- **Decision:** a small verifier (`oidc/jwt.rs`) on `ring` 0.17 (already in
  the tree through rustls): RS256/384/512, PS256/384/512, ES256, ES384.
  `none`, `HS*` and anything else refused; `crit` refused; the key must fit
  the algorithm (`kty`, `crv`, the key's `alg` if stated, `use` sig); key
  chosen by `kid`, or without `kid` only when exactly one key fits. Claims:
  `iss` exact, `aud` contains the client id, `azp` required and equal when
  several audiences (and equal whenever present), `exp`/`iat`/`nbf` with 60
  s leeway, `nonce` constant-time equal, `sub` 1-255 characters. Email,
  `email_verified` (bool or the string `"true"`) and name are read from the
  ID token only.
- **Why:** the needed subset is ~200 lines and fully unit tested; `ring`
  avoids a second crypto stack. Tests: RFC 7515 A.2's published RSA key
  (signature vector, plus signing test tokens with it) and ECDSA P-256 keys
  generated by ring, covering bad signatures, alg confusion, `none`/HMAC,
  wrong iss/aud/azp, expiry, nbf, iat, nonce, kid selection.
- **Rejected:** `jsonwebtoken` (pulls its own crypto choices and accepts
  HMAC unless configured carefully), `openidconnect` (large dependency tree,
  its own HTTP client abstraction); fetching UserInfo (an extra request for
  data most providers put in the ID token; documented as a limit).

### Account rules: identities by (issuer, subject); never sign into an account by email
- **Decision:** `engine_oidc_identities` keyed `UNIQUE (issuer, subject)`,
  plus `UNIQUE (user_id, issuer)` (one identity per provider per account).
  An identity that nobody linked never signs into an existing account, even
  with a verified matching email address: the owner signs in with their
  password and links the provider on `/account/profile/` (`POST
  /account/oidc/<name>/link/` with the session's CSRF token; the callback
  checks the same account is still signed in). Linking refuses an identity
  linked to someone else and a second identity of the same issuer.
- **New accounts** only with `create_accounts` (default: `[auth]
  registration = "open"`; needs `roles`, same reason as registration), only
  for a provider-verified address that no account uses, with
  `default_roles` (never `admin`, already enforced), no password
  (`unusable_hash`), address marked verified, counted against
  `max_registrations_per_address`, `welcome` message sent. A race making the
  same identity twice deletes the loser's account and signs in to the
  winner's.
- **Why:** providers differ in how well they verify addresses (some let
  users type any address), addresses get reassigned, and a provider anyone
  can register at would otherwise be a key to every account. Linking while
  signed in proves control of both sides.
- **Rejected:** auto-linking on verified email with a per-provider "trusted"
  flag (one misconfigured or compromised provider takes over any account,
  including administrators); linking by emailed confirmation (needs mail,
  which SQLite sites do not have).
- Tests: `crates/engine-auth/tests/oidc.rs` (both backends: takeover by
  email in several spellings, admin's address, unverified/missing email,
  creation with default roles, link conflicts, unlink rules, blocked
  accounts, rate limit, single-use and expiring attempts) and the
  end-to-end test below.

### Unlinking keeps a way in; blocked accounts; sessions; TOTP
- Unlinking is refused when the account has no usable password and no other
  identity of a *configured* provider (identities of removed providers do
  not count). The account row is locked first (`UPDATE ... WHERE id`), so
  two concurrent unlinks cannot both pass the check on Postgres; SQLite's
  `BEGIN IMMEDIATE` serialises anyway.
- Blocked accounts are refused (and their sessions were already deleted);
  the per-account failure limit applies as for passwords.
- Sign-in through a provider ends the browser's existing session and starts
  a new one exactly as password sign-in does (new token, `last_login_at`,
  members without admin rights do not land in `/admin/`).
- **TOTP:** an account with two-factor sign-in on still gets the TOTP step
  after a provider sign-in. The provider replaces the password, not the
  second factor the account owner chose; `require_totp_for_admins` keeps
  working unchanged. **Rejected:** trusting the provider's MFA (`amr`/`acr`
  claims are not standardised enough to rely on).

### Test provider: Dex v2.44.0 in `deploy/compose.oidc.yml`, started by both test scripts
- `wrought-test-oidc` on 127.0.0.1:59110 (issuer
  `http://127.0.0.1:59110/dex`), in-memory storage, three static users
  (password `password`, Dex's own example hash), a confidential client
  `wrought-test` and a public one `wrought-public`, redirect URIs for
  engines on localhost:59111-59119. The e2e test
  (`crates/engine-cli/tests/oidc.rs`) runs the engine on 59111 (Postgres)
  or 59112 (SQLite), so both suites may run at once, and drives Dex's real
  login form over HTTP. Skipped without `ENGINE_TEST_OIDC_ISSUER`.
- `scripts/lib.sh` gained `start_oidc`; `test-postgres.sh` and
  `test-sqlite.sh` call it (so the SQLite suite now needs Docker for this
  one test; `WROUGHT_NO_OIDC=1` skips it).
- **Rejected:** a mock provider written in Rust (would test our reading of
  the spec against itself); Keycloak (500+ MB image, slow start).

### Not done
- UserInfo endpoint, logout propagation (front/back-channel), `private_key_jwt`,
  `form_post`, per-provider allowed email domains, mapping provider groups
  to roles, setting a first password from the profile page (a reset link
  does it).
