+++
title = "Content types and the database"
description = "Content in PostgreSQL: type files, migrations, editing, revisions, taxonomy, blocks and safe rendering."
weight = 7
+++

Markdown files suit content a developer edits. Content that editors change
often, or that has structure (articles with authors and topics, products,
events) lives in the site's database instead. This needs an engine built with
the `cms` preset (`cargo build --release --features cms`), which adds the
`db-postgres`, `content` and `publish` features, and a PostgreSQL database.
The `static` preset has none of this code.

The [`examples/cms-site`](https://codeberg.org/kovirlabs/wrought/src/branch/main/examples/cms-site)
folder is a complete site to copy from (`engine new my-site --preset cms`).

## The database

```toml
[database]
url = "postgres://engine@localhost/example"   # or set ENGINE_DATABASE_URL
max_connections = 5
```

`ENGINE_DATABASE_URL` overrides `url`; use it to keep passwords out of the
site folder. For a local database, the repository has a Compose file:

```sh
docker compose -f deploy/compose.postgres.yml up -d --wait
export ENGINE_DATABASE_URL=postgres://wrought:wrought@127.0.0.1:55432/wrought
```

## Type files

Each content type is a TOML file in `types/`, named after the type:

```toml
# types/article.toml
label = "Article"
title_field = "title"     # default: a field named `title`
slug_from = "title"       # default: the title field

[[fields]]
name = "title"
type = "text"
required = true
max_length = 160

[[fields]]
name = "body"
type = "markdown"

[[fields]]
name = "author"
type = "relation"
to = "author"

[[fields]]
name = "topics"
type = "terms"
vocabulary = "topics"
```

Names are lowercase letters, digits and `_`, starting with a letter. Every
item also has `id`, `slug`, `status` (`draft` or `published`), `version` and
timestamps, so those names are reserved.

| Field type | Holds | Options |
| --- | --- | --- |
| `text` | plain text | `max_length` |
| `markdown` | Markdown, shown as sanitised HTML | `max_length` |
| `integer` | a whole number | `min`, `max` |
| `boolean` | true or false | |
| `date` | a date, `2026-11-20` | |
| `url` | an `http(s)://` or `mailto:` link, or a path starting with `/` | `max_length` |
| `select` | one of a list | `options` |
| `relation` | another item | `to` (the type) |
| `terms` | taxonomy terms | `vocabulary` |
| `blocks` | an ordered list of blocks | `blocks` (allowed block types) |

Every field takes `required`, `label` and `help` too. Validation runs on every
save and reports every bad field at once.

A type with `singleton = true` has exactly one item, for site settings or a
menu; templates read it with `get_singleton("settings")`.

### Blocks

A block type is a file in `types/blocks/` with fields (not relations, terms
or blocks). A `blocks` field stores a list such as
`[{ type = "quote", text = "...", cite = "..." }]`, and a template renders
each one with `render_block(b)`, which uses `templates/blocks/<type>.html`
with the block as `block`.

### Taxonomy

A `terms` field names a vocabulary. Terms are created when an item first uses
them; a term's slug comes from its name. A vocabulary can have its own pages
with a `taxonomy` route (below). Vocabulary names share cache tags with type
names, so they must differ.

## Migrations

Type files describe the tables; `engine types sync` turns changes into SQL
migrations and `engine migrate` applies them:

```sh
engine types sync     # writes migrations/postgres/0001_types_article_author.sql
engine migrate        # applies engine and site migrations, in order
engine migrate --status
```

- A new type creates a table `content_<type>`, with a trigger that records
  each change for publishing. A new field adds a column.
- Removing a field or type drops data, so it needs `--allow-drop`.
- Changing a field's type is refused: write that migration by hand (it
  usually converts data), then run `engine types sync --snapshot-only`.
- `migrations/types-snapshot.json` records what was last synced. Commit it
  with the migrations. `engine types sync --check` fails if they are behind
  the type files (useful in CI).

Every migration runs in a transaction and is recorded with a checksum;
editing one after it ran is an error. `engine build` and `engine serve`
refuse to start while migrations are pending or types are unsynced.

## Editing content

The admin UI arrives in a later release. Until then, `engine content` edits
through the same code path it will use:

```sh
engine content seed                                     # seed/*.toml
engine content list article [--all]
engine content create article --set title="Hello" --set body=@hello.md --publish
engine content set article hello --set title="Hello again"
engine content set article hello --unpublish
engine content set settings --set tagline="Made by hand"
engine content revisions article hello
engine content delete article hello
```

`--set field=@file` reads the value from a file. `terms` fields take a
comma-separated list; `blocks` fields take JSON.

### Versions and revisions

Every item has a `version`. A save names the version it edited, and if the
item has moved on since, the save fails with a conflict instead of
overwriting the other change:

```text
$ engine content set article 2 --set title="Stale" --expect-version 1
error: article #2 was changed by someone else: you edited version 1, but it
is now at version 3. Reload it and apply your change again
```

Without `--expect-version` the command uses the version it reads just before
saving. Each create, update and delete writes a revision row (the full item,
the author, the time) in the same transaction as the change itself.

### Seed files

`seed/*.toml` holds arrays of tables named after types. `slug` and `status`
are optional; relations name the target's slug. Seeding skips items whose
slug already exists, so it can run again safely.

```toml
[[author]]
slug = "ada"
name = "Ada"
status = "published"

[[article]]
title = "Hello"
author = "ada"
topics = ["Rust", "Web"]
status = "published"
```

## Routes for items and terms

```toml
[[routes]]
path = "/articles/{slug}/"    # or {id}
type = "article"
template = "article.html"

[[routes]]
path = "/topics/{term}/"
taxonomy = "topics"
template = "topic.html"
```

Each published item gets one page; templates see it as `page`, with its
fields plus `id`, `slug`, `url`, `title`, `date` (first published) and
`version`. Only published items are ever rendered; drafts have no page.

## Content in templates

| Function | Returns | Records tag |
| --- | --- | --- |
| `list_items(type, limit=, offset=, order=, term=)` | published items | `c:<type>` |
| `get_item(type, id_or_slug)` | one published item, or undefined | `i:<type>:<id>` |
| `get_singleton(type)` | the singleton's item, whatever its status | `s:<type>` |
| `list_terms(vocabulary)` | terms with `name`, `slug`, `url` | `c:<vocabulary>` |
| `render_block(b)` | the block's template, rendered | |

`order` is `newest` (default), `oldest`, `updated`, `title`, a field name, or
`-field` for descending. `term` filters by a term slug.

```jinja
{% for a in list_items("article", limit=5) %}
<h2><a href="{{ a.url }}">{{ a.title }}</a></h2>
{% endfor %}
{% set by = get_item("author", page.author) %}
{{ page.body | safe }}
```

## Safe output

Database content is never trusted, whoever wrote it:

- `markdown` fields are rendered and then cleaned by an allow-list sanitiser
  (ammonia): no scripts, event handlers, `style`, iframes or `javascript:`
  links. That is why inserting them with `| safe` is correct.
- `url` fields only hold `http(s)://`, `mailto:` or site paths, checked when
  saving and again when rendering.
- every other field is plain data, and templates escape it.
- slugs are checked by the database itself, so a slug set in SQL cannot put
  `..` or `/` into a published path.

Sanitising happens at render time, so a row edited directly in SQL is still
cleaned. Raw HTML in Markdown *files* is still passed through: the site's
developer wrote those.
