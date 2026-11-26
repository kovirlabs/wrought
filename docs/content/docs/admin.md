+++
title = "The admin, accounts and roles"
description = "Signing in, the admin UI, conflicts between editors, revisions, roles and per-field access, two-factor sign-in and password resets."
weight = 9
+++

Engines built with the `cms` preset have accounts (`auth`), roles (`roles`)
and a server-rendered admin (`admin`) at `/admin/`. A `static` engine has
none of it: no routes, nothing to probe.

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
address, and answers the same whether or not the account exists. In this
release there is no `mail` feature yet, so the link is written to the
server's log (`auth: password reset requested for ...`) for an operator to
pass on; the `mail` feature will send it instead. Setting a new password
signs the account out everywhere. Administrators can also send a link from
the account's admin page, and `engine user reset-link` prints one.

### Two-factor sign-in

Anyone can turn on two-factor sign-in on their profile page
(`/admin/profile/`): add the key to an authenticator app and enter the code
it shows. From then on, signing in also asks for the current code; codes
cannot be reused. `[auth] require_totp_for_admins = true` makes it compulsory
for administrators. A lost device: `engine user totp-off --email ...` or the
account's admin page.

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
change, read only, or hidden. It can also allow managing terms, and managing
accounts and roles.

Rights are checked on the server for every request, not just in the forms:
a read-only field has no input, a hidden field is not shown anywhere
(including history), and a request that tries to change either anyway, for
example a hand-made `POST`, is refused with nothing saved. Only
administrators can give administrator roles or change administrators'
accounts, and the last active administrator cannot be removed.

## Pages for signed-in people

Routes can use the `request` render mode and `requires = "auth"`; see
[the render dial](/docs/render-dial/#what-request-does). The example site's
`/account/` page is one.
