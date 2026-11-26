+++
title = "The admin, accounts and roles"
description = "Signing in, registration, the admin UI, conflicts between editors, revisions, review before publishing, roles and per-field access, two-factor sign-in and password resets."
weight = 9
+++

Engines built with the `cms` or `app` preset have accounts (`auth`), roles
(`roles`), a server-rendered admin (`admin`) at `/admin/` and review before
publishing (`workflow`). A `static` engine has none of it: no routes,
nothing to probe.

## The first administrator

There are no default credentials. Create the first account from the command
line; it becomes an administrator:

```sh
engine user create --site my-site --email you@example.org --name "You"
```

It prints a single-use link to choose a password (valid one hour). On a
local server, open it with the address `engine serve` prints. To set the
password directly instead, pipe it in: `... --password-stdin`.

Then run `engine serve` and open `/admin/`.

## Signing in

`/account/login/` asks for email and password. Passwords are hashed with
Argon2id and need at least 12 characters (length, not symbols, makes them
strong). Sessions are stored in the database; the browser only holds a
random token in a cookie that is `Secure`, `HttpOnly`, `SameSite=Lax` and
`__Host-` prefixed. Sessions end after `[auth] session_hours` (12) or
`idle_minutes` (120) without a request, and on sign-out.

After 5 failed sign-ins for an account (or 30 from one address) within 15
minutes, sign-in is refused for a while, even with the right password.

Every form carries a CSRF token, and a `POST` whose `Origin` names another
site is refused.

### Forgotten passwords

`/account/reset/` sends a single-use link (one hour) to the account's
address, and answers the same whether or not the account exists. With the
`mail` feature and an SMTP server it is an email (template
`password_reset`, see [jobs and mail](/docs/jobs-and-mail/)); otherwise the
link is written to the server's log (`auth: password reset requested for
...`) for an operator to pass on, so treat that log as secret. Setting a
new password signs the account out everywhere. Administrators can also send
a link from the account's admin page, and `engine user reset-link` prints
one.

### Registration and the profile page

With `[auth] registration = "open"` (and the `roles` feature) visitors
create accounts at `/account/register/`, or through the `register_form`
component on a page of your own. New accounts get `[auth] default_roles`
(none by default, so no admin access), are signed in at once and get a
`welcome` email. Registration has a honeypot field and a limit per client
address (`max_registrations_per_address`, 5 an hour). Every signed-in
person can change their name and password at `/account/profile/` (or the
`profile_form` component). Members without admin rights land on the site,
not the admin, after signing in.

### Two-factor sign-in

Anyone with admin access can turn on two-factor sign-in on their profile
page (`/admin/profile/`): scan the QR code (drawn by the server; the secret
goes to no one else) or type the key into an authenticator app, and enter
the code it shows. From then on, signing in also asks for the current code;
codes cannot be reused. `[auth] require_totp_for_admins = true` makes it
compulsory for administrators. A lost device: `engine user totp-off --email
...` or the account's admin page.

Two-factor secrets are stored encrypted (ChaCha20-Poly1305) with a site key
that is not in the database: set `ENGINE_SECRET_KEY` to 32 or more random
characters (`openssl rand -base64 48`), or point `[auth] secret_key_file` at
a file holding it. Without a key, two-factor sign-in cannot be turned on
(the profile page says so), `require_totp_for_admins` is refused, and
`engine serve` will not start while any account uses it. Secrets stored by
0.3 are encrypted the first time the server starts with a key. Keep the key
with your other secrets: losing it means turning two-factor sign-in off and
on again for every account.

## Editing content

The admin lists the items of every type in `types/*.toml` (search, status
filter, 25 per page) and edits them with forms generated from the type:
text, Markdown, numbers, dates, links, selects, relations (a list of the
target's items), terms (comma-separated) and blocks (add, remove, reorder).
Publishing an item, changing it or deleting it rewrites the published pages
within a moment, exactly as with `engine content`.

Everything works without JavaScript. With it, the bundled htmx (served from
your own site, like TagBrush and TagAction) swaps pages without full reloads
and filters lists as you type.

### When two people edit the same item

Each form remembers the version it was opened at. If someone else saved the
item in between, saving does not overwrite their work. Instead you get a
conflict page that:

- says who saved, when, and which version it is now;
- shows, field by field, your value next to the current one, and marks fields
  you both changed;
- keeps all your changes in the form.

Choose **Save my version** to replace the current version with yours
(knowingly; if it changed yet again, you are asked again), or reload to
keep theirs. Deleting and restoring check versions the same way.

### History

Every save writes a revision with the whole item, who saved it and when.
The item's **History** page lists them; open one to see its values and
**Restore** it, which saves those values as a new version (slug and status
stay as they are).

### Review before publishing

With `workflow` (in the `cms` and `app` presets), people whose roles let
them edit a type but not publish it send a draft for review from its edit
page, with a note. Everyone who may publish that type is notified (email
with `mail`, otherwise the server log) and finds it under **Review**. They
open **Preview**, which shows the draft in the site's own page template
(never cached, `noindex`), then **Approve and publish** (a version-checked
save: if someone changed the item meanwhile, nothing is published) or
**Send back for changes** with a note. The author is notified either way,
and can withdraw a request. The item's history of requests and decisions is
on its edit page.

### Taxonomy

Terms are created when an item first uses them. **Taxonomy** renames terms
(their address stays) and deletes them.

## Accounts and roles

Administrators manage accounts under **Accounts**: create one (with a
password, or without and the owner gets a link), block it, change its roles,
send a reset link, turn off two-factor sign-in.

A user can do what their roles allow together. Two roles exist from the
start:

| Role | Can |
| --- | --- |
| `admin` | everything; cannot be edited or deleted |
| `editor` | view, create, edit, delete and publish every type; manage terms |

**Roles** makes more. A role grants, per content type (or for "every other
type"): view, create, edit, delete and publish; and per field: read and
change, read only, or hidden. It can also allow managing terms, managing
accounts and roles, and (with `forms`) managing forms and reading their
submissions.

Rights are checked on the server for every request, not just in the forms:
a read-only field has no input, a hidden field is not shown anywhere
(including history), and a request that tries to change either anyway, for
example a hand-made `POST`, is refused with nothing saved. Only
administrators can give administrator roles or change administrators'
accounts, and the last active administrator cannot be removed.

## Forms

With `forms`, the admin also lists forms and their submissions and builds
new forms; see [forms](/docs/forms/#in-the-admin).

## Pages for signed-in people

Routes can use the `request` render mode and `requires = "auth"`; see
[the render dial](/docs/render-dial/#what-request-does). The CMS example's
`/account/` page and the small app's `/me/` are such pages; the
[Accounts and App UI components](/docs/app-components/) build them.
