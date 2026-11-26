+++
title = "Forms"
description = "Forms from files or the admin, server-side validation, stored submissions, a spam guard without captchas, receipts by email."
weight = 12
+++

Engines built with the `app` preset (or the `forms` feature, which needs a
database) show forms on pages, check what people send on the server, store
it, and keep bots out without a third-party captcha. With `mail`, the sender
gets a receipt and your team a notice.

## A form is a file

```toml
# forms/support.toml
title = "Support request"
intro = "Tell us what is wrong. You get a receipt by email."
submit = "Send request"
success = "Thank you. Your request is in."
# redirect = "/thanks/"          # go there instead of showing `success`
# store = true                   # keep submissions (the default)
# unique = "email"               # keep each value of this field once

[receipt]                        # email the sender (needs `mail`)
to_field = "email"               # an `email` field
# template = "form_receipt"      # templates/mail/<template>.txt/.html

[notify]                         # email your team
to = ["support@example.org"]

[spam]
min_seconds = 3                  # faster than this is a bot
max_per_hour = 10                # per form and client address

[[fields]]
name = "email"
label = "Your email address"
type = "email"
required = true

[[fields]]
name = "topic"
type = "select"
options = ["Billing", "Bug", "Question"]

[[fields]]
name = "message"
type = "textarea"
required = true
min_length = 10
max_length = 4000
```

Field types: `text`, `email`, `textarea`, `number`, `select`, `radio`,
`checkbox`, `date`, `url`, `tel` and `file` (below). Each field can have
`label`, `required`, `min_length` / `max_length`, `min` / `max` (numbers),
`options` (select and radio), `help`, `placeholder` and `autocomplete`. The
file is checked when the engine starts: an unknown key, a select without
options or a receipt field that is not an `email` field is an error.

### File uploads

With the `media` feature a field can take a file:

```toml
[[fields]]
name = "portfolio"
type = "file"
accept = ["image", "pdf"]   # the default; or just one
max_mb = 5                  # at most `[media] max_mb`
required = true
```

The form is sent as `multipart/form-data` (the components set that up).
Each file is checked like every upload, by its content (see
[media](/docs/media/)): the wrong kind of file, or too big a one, comes back
as a field error with the other values kept. Files are stored only when the
whole submission is valid, as **private** media: never served at `/media/`,
downloadable from the submission in the admin by people who may read
submissions. The submission records `{media_id, name, type, bytes}`. A form
takes at most five files, and file fields cannot be in multi-step forms.

The engine has two forms built in, `contact` and `newsletter`, so the
components below work without a file; a `forms/contact.toml` of your own
replaces the built-in one. With `media` there is a third, `upload` (name,
email, one picture or PDF, a note), behind the `file_upload` component.

## Showing a form

Put a component on a page whose route is rendered per request:

```toml
[[routes]]
path = "/support/"
template = "support.html"
mode = "request"
```

```jinja
{{ component("form", name="support") }}
```

| Component | Shows |
| --- | --- |
| `form` | any form, by `name` |
| `contact_form` | the `contact` form (name, email, message; receipt) |
| `newsletter_signup` | the `newsletter` form (email, consent; each address once) |
| `multi_step_form` | a form with `[[steps]]`, one step at a time |
| `file_upload` | with `media`: the `upload` form, or any form with file fields (`name=`); each file input has its label and its accepted kinds in the help text, and TagAction lists the chosen file with a thumbnail for pictures and takes drag and drop |

Forms need `mode = "request"` because each visitor gets their own security
token; on a `build` or `publish` page the engine stops with a message saying
so. The components render plain HTML: labels, `required`, `aria-invalid` and
`aria-describedby` for errors, a fieldset for radio buttons. They work
without JavaScript; TagAction adds its usual behaviour.

## What happens on submit

The form posts to `/_engine/forms/<name>/`. The engine:

1. checks the CSRF token (the session's for signed-in visitors, a cookie's
   for others) and refuses posts from other sites;
2. runs the spam guard (below);
3. validates every field on the server: required, lengths, number ranges,
   choices, email, date, URL and phone shapes;
4. stores the submission (with the account, when signed in), and
5. redirects to `redirect`, or back to the page with `?sent=<name>`, where
   the form shows its `success` message.

If something is wrong, the page is shown again at once with the values and
the messages (status 422; 429 when rate limited).

## The spam guard

No captcha, no outside service:

- **Honeypot.** A field people never see (`website`). Bots fill it in; the
  engine answers as if all was well and stores nothing.
- **Time trap.** The form carries a stamp of when it was shown, signed with
  a key the engine keeps in the database. Sent faster than `min_seconds`,
  with a forged stamp, or after `max_age_hours` (24): refused.
- **Rate limit.** At most `max_per_hour` accepted submissions per form and
  client address.

Every refusal is logged with its reason and counted in the admin's forms
list. Behind a proxy, set `[auth] behind_proxy = true` so the limit sees the
real client address.

## Multi-step forms

```toml
[[steps]]
title = "Your team"
fields = ["team", "size"]

[[steps]]
title = "Contact"
fields = ["email", "terms"]
```

**Next** checks the step's fields and shows the next step; earlier answers
travel in the form itself, so nothing is stored until the last step, which
checks every field again.

## Receipts and notifications

With `mail` configured, `[receipt]` emails the address in `to_field` the
`form_receipt` template (the form's title, the submission number and every
answer), and `[notify]` emails `form_notification` to your team. Both go
through the [job queue](/docs/jobs-and-mail/), so a slow mail server never
slows the page. Override the templates in `templates/mail/`.

## Your own submissions

On a request page, `my_submissions(form="support")` lists the signed-in
visitor's own submissions (newest first, fields flattened for tables); for
anyone else it is empty. The small-app example's account page shows them
with the data table, stat card and chart components.

## In the admin

With the `admin` feature, **Forms** (for administrators and roles with the
*manage forms* right) lists every form with its submissions and refused
spam, shows each submission, deletes them, and downloads all as CSV (cells
that a spreadsheet would run as formulas are defused). **New form** builds a
form in the browser (fields added, moved and removed with buttons, no
JavaScript needed); such forms are stored in the database and
version-checked like content. Forms from files are read-only there.
