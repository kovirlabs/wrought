+++
title = "Getting started"
description = "Install the engine, create a site and run it."
weight = 1
+++

## Install

Wrought is a single binary called `engine`. Until release binaries are
published, build it from source with a stable Rust toolchain (1.85 or later):

```sh
git clone https://codeberg.org/kovirlabs/wrought
cd wrought
cargo build --release
# the binary is target/release/engine; put it on your PATH
cp target/release/engine ~/.local/bin/
```

## Create a site

```sh
engine new my-site --preset static
cd my-site
engine dev
```

`engine dev` builds the site, serves it at <http://127.0.0.1:4400/>, and
rebuilds and reloads the browser whenever you save a file.

The `static` preset copies the static-blog example. The `cms` and `app`
presets arrive with the features they need.

## What is in a site

```text
my-site/
  site.toml        settings: [site], [render], [[routes]], [ui]
  content/         Markdown pages; folders become sections
  templates/       your layouts and component overrides (optional)
  static/          files copied as they are: images, fonts, robots.txt
  public/          the build output (do not edit; do not commit)
```

Only `site.toml` is required. A site without `templates/` uses the layouts
and components built into the engine.

## Build and publish

```sh
engine build          # writes public/
engine serve          # serves public/ at http://127.0.0.1:4400/
engine check          # checks the HTML with TagCheck
```

`public/` is plain files. Serve it with any web server; the
[deployment page](/docs/deployment/) shows Caddy.

## A site with a database

For content that editors change, build the engine with the `cms` preset and
start from the CMS example (it needs PostgreSQL; see
[content types](/docs/content-types/#the-database)):

```sh
cargo build --release --features cms
engine new my-cms --preset cms
cd my-cms
export ENGINE_DATABASE_URL=postgres://...
engine migrate && engine content seed && engine build
engine serve          # serves public/ and publishes content changes
```

## A web app

For accounts, forms, background jobs and email, build the `app` preset and
start from the small app (PostgreSQL again, plus an SMTP server for mail;
`deploy/compose.mailpit.yml` runs a local one that delivers nothing):

```sh
cargo build --release --features app
engine new my-app --preset app
cd my-app
export ENGINE_DATABASE_URL=postgres://...
export ENGINE_SMTP_URL=smtp://127.0.0.1:51025
export ENGINE_SECRET_KEY="$(openssl rand -base64 48)"
engine migrate && engine content seed && engine build
engine serve          # pages, sign-in and registration, forms, job workers
```

Open `/register/`, create an account, send a support request at
`/support/`, and find the receipt in the mail sink. See
[forms](/docs/forms/), [jobs and mail](/docs/jobs-and-mail/) and
[Accounts and App UI components](/docs/app-components/).
