+++
title = "Generators and routes"
description = "engine generate scaffolds content types, routes, forms, handlers, jobs and mailers; engine routes lists every URL a site answers."
weight = 22
+++

`engine generate` writes the files a new piece of a site needs, in the
layout the rest of these docs use, and `engine routes` lists everything the
site answers. Both work offline: no database, nothing rendered.

## engine generate

```sh
engine generate type event starts:date! format:select:talk|workshop summary:markdown
engine generate route /about/team/ --title "Our team"
engine generate form rsvp email:email! guests:number
engine generate mailer reminder when:string
engine generate app              # make the site its own Rust crate
engine generate handler hello    # these three need an app crate
engine generate handler signup --method post --path /signups/
engine generate job report.daily user_id:i64 note:string
```

Every generator:

- **refuses to replace a file that exists** and writes nothing in that
  case; `--force` replaces them. Files it adds to (`site.toml`, the app's
  `lib.rs`, a module's `mod.rs`, `.gitignore`) are edited, not replaced.
- **`--dry-run`** lists what it would create and edit, and writes nothing.
- **checks what it wrote**: `site.toml` must still load, a type must load
  with the others and get its migration. A failed check puts every file
  back as it was.
- takes `--site <folder>` like every command (default: the current folder).

The files are made from templates built into the engine binary. Generated
pages extend the site's `layout.html` when it has one (the app preset's),
else `base.html`. Rust files are formatted with `rustfmt` when it is on
`PATH`.

### Fields

Types and forms take fields as `name[:type[:arg]][!]`; `!` makes the field
required, and `arg` is what the type needs:

| Generator | Types | `arg` |
| --- | --- | --- |
| `type` | `text` (default), `markdown`, `integer`, `boolean`, `date`, `url`, `select`, `relation`, `terms`, `blocks`, `media`, `token` | `select:a\|b`, `relation:<type>`, `terms:<vocabulary>`, `blocks:<block>\|<block>`, `token:<group>` |
| `form` | `text` (default), `email`, `textarea`, `number`, `select`, `radio`, `checkbox`, `date`, `url`, `tel`, `file` | `select:a\|b`, `radio:a\|b` |
| `job`, `mailer` | `i64`, `f64`, `bool`, `string` | |

(Quote a spec with `|` in the shell: `'format:select:talk|workshop'`.)

### What each one writes

| Command | Writes |
| --- | --- |
| `generate type <name> [fields]` | `types/<name>.toml`; `templates/<name>.html` (an item) and `templates/<plural>.html` (the list, with `list_items`); two `publish` routes in `site.toml`, `/<plural>/` and `/<plural>/{slug}/`; the migration (`migrations/postgres/NNNN_types_<name>.sql`, exactly as `engine types sync` writes it) |
| `generate route <path>` | a `[[routes]]` entry and its template (`--template` names an existing one to use instead; `--mode`, `--title`) |
| `generate form <name> [fields]` | `forms/<name>.toml` (with spam limits), `templates/<name>.html` with the form component, a `request` route at `/<name>/` |
| `generate mailer <name> [fields]` | `templates/mail/<name>.txt` and `.html`; in an app crate also `src/mailers/<name>.rs`: a `send` function and a data struct, with a test that renders the templates |
| `generate app` | `Cargo.toml`, `src/lib.rs` (the `App` with the registrations), `src/main.rs`, `tests/app.rs`, `/target/` in `.gitignore` |
| `generate handler <name>` | `src/handlers/<name>.rs` registered with `App::route`, `tests/handler_<name>.rs`; for `GET` (the default) a template it renders, for `--method post\|put\|patch\|delete` a JSON handler with the same-origin check |
| `generate job <kind> [fields]` | `src/jobs/<module>.rs` with the payload struct and a test, registered with `App::job` |

A type without a `title` field gets one: the item page and the list show
it. Default fields: `title! body:markdown` for a type, `name! email:email!
message:textarea!` for a form, `id:i64` for a job's payload. A type's
migration includes any other unsynced change to `types/*.toml`, as `types
sync` would; the generator says so when there is one. In a build without
the `content` feature, it writes the files and leaves the migration to
`engine types sync` in a cms or app build.

### Handlers and jobs need an app crate

Handlers, jobs and the sender of a mail are Rust, so they live in the
site's own crate ([app crates](/docs/app-crates/)). On a site that is not
one, `generate handler` and `generate job` stop with a message pointing at
`engine generate app`; nothing is converted without being asked.
`generate mailer` writes the templates and says how to get the sender.

`engine generate app` makes the site folder the crate's folder:

```sh
engine generate app                         # engine-cli from crates.io, features ["app"]
engine generate app --features jobs,mail    # a smaller engine: only what the site uses
engine generate app --engine-path ../hammerwright/crates/engine-cli
cargo test
cargo run -- routes
```

The registrations go into the file with `App::new()` (`src/lib.rs` for a
generated crate, `src/main.rs` for one written by hand like
`examples/custom-app`), right after it; module declarations (`pub mod
handlers;`) are added once. Each generated test runs without a database:
a handler's test asks the crate's own binary for its routes
(`routes --json`) and checks the handler is registered with its method; a
job's test reads a sample payload; a mailer's test renders its templates
with sample data. To drive a handler over HTTP against a throwaway
database, see [testing an app crate](/docs/app-crates/#testing-an-app-crate).

## engine routes

```sh
engine routes [--json]
```

lists every route of the site:

| Kind | From | Shows |
| --- | --- | --- |
| `page` | `content/` files and `[[routes]]` | method, path, render mode, template, source, cache tags |
| `pattern`, `list` | `[[routes]]` with `type`, `taxonomy` or `list` | the pattern (`/events/{slug}/`) and the tags its pages carry (`i:event:*`) |
| `handler` | the app crate's `App::route` | methods and path |
| `form` | `forms/*.toml`, the built-in forms | `POST /_engine/forms/<name>/` |
| `engine` | the features of this build | the account pages, the admin, the API, media, health... |

```text
METHOD    PATH                    KIND     MODE     TEMPLATE      SOURCE                    TAGS
GET       /                       page     publish  home.html     site.toml [[routes]] #1   c:signups s:site
GET       /notes/{slug}/          pattern  publish  note.html     site.toml [[routes]] #2   i:note:*
POST      /signups/               handler  -        -             app
GET       /signups/{id}/          handler  -        -             app
GET,POST  /account/login/         engine   -        -             engine (auth)
```

- **Cache tags** are those each page recorded in the last `engine build`
  (read from the build's `.engine-manifest.json`); before a first build the
  column is empty and a note says so. `request` pages are not built, so
  their tags show only in a running server.
- **Handlers** are known to the binary that registered them: run `routes`
  with the site's own binary (`cargo run -- routes`). The stock `engine`
  binary notes that the site is an app crate.
- A route whose mode or feature this build lacks says so in its line
  instead of failing.
- `--json` prints `{"routes": [...], "notes": [...]}`, each route with
  `kind`, `methods`, `path`, `mode`, `template`, `source`, `tags` and
  `note`.
