+++
title = "Jobs and mail"
description = "Background jobs on PostgreSQL with retries and schedules, and email sent through them."
weight = 13
+++

The `jobs` feature runs work outside the request that asked for it; `mail`
(which needs `jobs`) sends email through it. Both are in the `app` preset and
run on the site's PostgreSQL (`FOR UPDATE SKIP LOCKED` and `LISTEN/NOTIFY`;
see [databases](/docs/databases/)). Administrators watch the queue, and retry or
delete failed jobs, under **Jobs** in [the admin](/docs/admin/#jobs).

## Jobs

Jobs live in the database, in `engine_jobs`. Workers claim them with
`FOR UPDATE SKIP LOCKED`, so any number of workers, in one process or many,
share the queue and never run the same job twice.

`engine serve` runs workers next to the web server. To run them elsewhere
(another machine, another container), start the server with
`--no-workers` and run `engine jobs work` there.

| Situation | What happens |
| --- | --- |
| A job fails (error or panic) | Retried after a growing pause (10 s, 20 s, 40 s... with some jitter, at most an hour) |
| It keeps failing | After `max_attempts` (5) it is `dead`: kept, shown by `engine jobs list --status dead`, retried by hand with `engine jobs retry <id>` |
| Its worker dies mid-job | The worker renews a lease while it runs; once the lease runs out, another worker takes the job (that counts as an attempt) |
| The server stops | Workers take no new jobs, let running ones finish for `shutdown_grace_seconds`, then put the rest back in the queue |

Delivery is *at least once*: a worker cut off from the database can have its
job run again elsewhere after the lease. Jobs should be safe to repeat.

```toml
[jobs]
workers = 4                  # jobs at the same time, per process (unset: CPUs, 2 to 16)
poll_seconds = 2             # new jobs also wake workers at once (NOTIFY)
lease_seconds = 60
shutdown_grace_seconds = 30
retry_base_seconds = 10
retry_max_seconds = 3600

[[jobs.schedule]]
name = "nightly-report"
job = "site.report"
every = "24h"                # s, m, h or d
payload = { to = "office@example.org" }
```

A schedule enqueues its job once per interval, however many processes run.
The engine schedules `engine.cleanup` hourly: it deletes finished jobs after
a week, dead ones after a month, and old form events.

## Mail

```toml
[mail]
from = "Example <hello@example.org>"
reply_to = "office@example.org"     # optional
max_attempts = 8
timeout_seconds = 20
```

The SMTP server comes from `ENGINE_SMTP_URL` (or `[mail] smtp_url`; keep
passwords out of the site folder):

| URL | Means |
| --- | --- |
| `smtp://127.0.0.1:1025` | no TLS; for a local relay or test sink only |
| `smtp://user:password@mail.example.org:587?tls=required` | STARTTLS |
| `smtps://user:password@mail.example.org:465` | TLS |

Without a server the engine says so at start; account messages go to its
log, and queued mail waits.

Messages are templates: `templates/mail/<name>.txt` with a `subject` and a
`body` block, and optionally `templates/mail/<name>.html` for the HTML part.
A site's file replaces the built-in one. Templates get the message's data
plus `site_name`, `site_url` and `to_name`.

```jinja
{% block subject %}Your order {{ number }}{% endblock %}
{% block body %}
Hello {{ to_name }},

thank you for your order.
{% endblock %}
```

| Built-in template | Sent when |
| --- | --- |
| `password_reset` | someone asks for a reset link |
| `welcome` | an account is registered |
| `form_receipt` / `form_notification` | a form with `[receipt]` / `[notify]` is sent |
| `workflow_review` / `workflow_decision` | a draft is sent for review / approved or sent back |

A message is rendered when it is asked for (so a template error shows up in
the request that caused it), then queued as a `mail.send` job; a worker
hands it to the SMTP server. After delivery the job keeps no copy of the
text, so reset links do not linger in the database.

## A local mail sink

`deploy/compose.mailpit.yml` runs Mailpit, which accepts every message and
delivers none:

```sh
docker compose -f deploy/compose.mailpit.yml up -d --wait
export ENGINE_SMTP_URL=smtp://127.0.0.1:51025
# messages: http://127.0.0.1:58025/
```
