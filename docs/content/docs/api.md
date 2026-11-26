+++
title = "JSON API"
description = "Read and write content over HTTP with scoped, revocable tokens; roles, field rights and version checks as in the admin."
weight = 18
+++

The `api` feature (in the `app` preset) answers `/api/v1/` with JSON. It
needs accounts and content: a token acts *as an account*, with that
account's roles and per-field rights, narrowed by the token's scopes.

## Tokens

Make one in the admin under **API tokens**, or on the command line:

```sh
engine api token create --email you@example.org --name "nightly import" \
    --scope read --scope write:article --days 90
```

The token (`wr_` and 43 more characters) is shown once; only its SHA-256 is
stored. Scopes are `read`, `write` (write includes read), or per type:
`read:article`, `write:article`. Tokens can expire, are revoked in the admin
or with `engine api token revoke <number>`, and stop working when their
account is blocked. Administrators see and revoke everyone's tokens.

Send it with every request:

```sh
curl -H "Authorization: Bearer $TOKEN" https://example.org/api/v1/
```

Cookies are ignored (so there is nothing for cross-site requests to ride
on), and no CORS headers are sent: the API is for programs and servers, not
for scripts on other sites.

## Endpoints

| Method and path | Does |
| --- | --- |
| `GET /api/v1/` | the account, the token's scopes, the types it may read and what it may do with them |
| `GET /api/v1/content/<type>/` | a page of items: `page`, `per_page` (1 to 100, default 20), `status` (`published`, `draft`, `all`), `order`, `term`, `q` |
| `GET /api/v1/content/<type>/<id or slug>` | one item, with its version as `ETag` |
| `POST /api/v1/content/<type>/` | create: `{"fields": {...}, "slug": "...", "status": "draft"}` |
| `PATCH /api/v1/content/<type>/<id or slug>` | change the given fields (plus `slug`, `status`) |
| `DELETE /api/v1/content/<type>/<id or slug>` | delete |

Lists answer `{"data": [...], "page", "per_page", "total", "pages", "next",
"prev"}` with a `Link` header. An item is `{"type", "id", "slug", "status",
"version", "created_at", "updated_at", "published_at", "url", "fields"}`;
fields are what the admin edits (Markdown as written, terms as names,
relations and media as numbers), minus the ones the account may not read.

## Writes are version-checked

Changes and deletes must say which version they were based on, with
`If-Match: "3"` (the `ETag` of the item) or `"version": 3` in the body (for
`DELETE`, `?version=3`). Without it the answer is **428**; with an old one,
**409** with the current version, and nothing is saved: two programs (or a
program and an editor) never overwrite each other silently.

```sh
curl -X PATCH -H "Authorization: Bearer $TOKEN" -H 'If-Match: "3"' \
     -H 'Content-Type: application/json' \
     -d '{"fields": {"title": "Why wrought iron endures"}}' \
     https://example.org/api/v1/content/article/why-wrought-iron-lasts
```

Writing a field the account may not write is a **403** `field_forbidden`
naming the fields; publishing or unpublishing (`status`) needs the publish
right. Every save writes a revision, attributed to the account and the
token.

## Errors

Always the same shape, with the HTTP status repeated:

```json
{"error": {"status": 409, "code": "conflict", "message": "...", "current_version": 4}}
```

Codes: `unauthorized` (401), `forbidden`, `field_forbidden` (403),
`not_found`, `unknown_type` (404), `version_required` (428), `conflict`
(409), `invalid` (422, with `fields`), `bad_json`, `bad_query`,
`bad_version` (400), `rate_limited` (429), `internal` (500).

## Rate limits

```toml
[api]
requests_per_minute = 120     # per token
bad_tokens_per_minute = 30    # per client address, requests with a missing or bad token
behind_proxy = false          # take the address from X-Forwarded-For
```

Over the limit the answer is 429 with `Retry-After`. Counts are kept in
memory per `engine serve` process.
