+++
title = "Accounts and App UI components"
description = "Login, register, password reset, profile and account menu; data table, stat card, chart, alerts, modal, toast and empty state."
weight = 14
+++

Two groups of the [component library](/components/) are for pages rendered
per request (`mode = "request"`): they show the signed-in visitor or carry
a per-visitor form token. They are compiled in with the features that need
them (`auth`, and the `request` render mode), render semantic HTML that
works without JavaScript, and follow the TagBrush styles. As with every
component, a file with the same name in the site's `templates/components/`
replaces the built-in one.

## Accounts

| Component | Shows | Arguments |
| --- | --- | --- |
| `login_form` | email and password, posting to `/account/login/` | `next` (default: this page), `title`, `level` |
| `register_form` | name, email, password and a honeypot (when `[auth] registration = "open"`) | `next`, `title`, `level` |
| `reset_form` | asks for a reset link by email | `title`, `level` |
| `profile_form` | change name and password | `next`, `level` |
| `account_menu` | the visitor's name with Profile, Admin (if allowed) and Sign out in a `<details>` menu; or Sign in / Create an account | `links` (extra `[{label, url}]`), `label` |

```jinja
{% extends "base.html" %}
{% block header %}
{{ component("header", end=component("account_menu", links=[{"label": "Your account", "url": "/me/"}])) }}
{% endblock %}
```

On a page that is not rendered per request (no visitor, no token), the
forms turn into links to the engine's own pages, and the account menu shows
the sign-in link; for signed-in visitors every page is rendered per request
anyway (see [the render dial](/docs/render-dial/#what-request-does)). The
template global `accounts` has the routes (`login_url`, `register_url`,
`reset_url`, `profile_url`, `logout_url`) and `registration`.

## App UI

| Component | Shows | Arguments |
| --- | --- | --- |
| `data_table` | a table with sortable columns, a filter box and pages, done on the server through links and a GET form | `rows`, `columns` (keys or `{key, label, num, sort}`), `id` (names the query parameters), `per_page`, `caption`, `filter`, `empty` |
| `stat_card` | a label and a number (`<data>`), an optional change | `label`, `value`, `unit`, `delta`, `trend` (`up`/`down`), `href`, `level` |
| `chart` | a bar or line chart drawn as SVG on the server, with the numbers in a table under a disclosure | `labels`, `series` (`[{name, values}]` or numbers), `kind`, `title`, `caption`, `width`, `height` |
| `alert` | a message; errors and warnings with `role="alert"`, others `role="status"` | `text`, `title`, `kind` (`info`, `success`, `warning`, `error`) |
| `modal` | a native `<dialog>` opened by an invoker button (`commandfor`; TagAction fills in for older browsers) | `id`, `title`, `text`, `label`, `close`, `href` (a page with the same content) |
| `toast` | a short confirmation announced politely | `text`, `kind` |
| `empty_state` | what to do when a list is empty | `title`, `text`, `href`, `action`, `level` |

```jinja
{% set mine = my_submissions(form="support") %}
{{ component("stat_card", label="Support requests", value=mine | length) }}
{{ component("data_table", rows=mine, id="requests", caption="Your requests",
   columns=[{"key": "id", "label": "#", "num": true}, {"key": "created_at", "label": "Sent"},
            {"key": "topic", "label": "Topic"}]) }}
{{ component("chart", labels=["Mon", "Tue"], series=[{"name": "Orders", "values": [3, 5]}],
   caption="Orders this week") }}
```

The data table reads `request.args` and keeps the page's other query
parameters in its links, so several tables can share a page. The helpers
behind it are template functions too: `table_view(rows, columns, ...)` and
`chart_svg(labels, series, ...)`.

## Forms

The Forms components (`form`, `contact_form`, `newsletter_signup`,
`multi_step_form`) are described on the [forms page](/docs/forms/).
