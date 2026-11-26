+++
title = "Starter themes"
description = "Three complete starting points built only from the component library: documentation, business site and app dashboard."
weight = 13
+++

A starter theme is a finished site to start from, built only from the
[built-in components](/docs/components/) and styled only through design
tokens. `engine new` lays it over a preset's example site:

```sh
engine new handbook --preset static --theme docs
engine new workshop --preset cms --theme business
engine new console --preset cms --theme dashboard
engine new my-site --theme list        # what there is
```

| Theme | Presets | What you get |
| --- | --- | --- |
| `docs` | static, cms, app | A landing page (hero, cards, call to action) and a `docs/` section laid out with sidebar navigation (`section_nav`), breadcrumbs, an "On this page" table of contents (`toc`) and previous/next links. Three sample pages. |
| `business` | static, cms, app | A home page with a hero, services (`grid` of `card`s), an about band (`columns`), testimonials (`quote`), a history (`timeline`) and a call to action; a contact page. |
| `dashboard` | cms, app | A `/dashboard/` page for signed-in people (`mode = "request"`, `requires = "auth"`) with stat cards, a bar chart, a sortable and filterable data table, an alert, tabs, an empty state and a dialog, and the account menu in the header. Sample numbers to replace with your own queries. |

The dashboard needs the `cms` or `app` preset because its App UI and
Accounts components render per request for a signed-in visitor; `engine
new --preset static --theme dashboard` says so and stops.

## What a theme changes

A theme is a folder in the engine's source, `themes/<name>/`:

- `files/`: templates and Markdown pages copied over the preset's
  (`templates/home.html`, `templates/docs.html`, `content/docs/...`);
- `theme.toml`: its design tokens, which replace the preset's `[ui.tokens]`;
  navigation entries, put in front of the preset's `nav`; and routes
  appended to `site.toml` (the dashboard's request route).

After `engine new` everything is ordinary site files: change the words,
the tokens and the templates as you like. Components keep working the same
way they do anywhere else: a template with the same name in
`templates/components/` replaces the built-in one.

## Checked

Every combination of preset and theme is created, built and (for the
dashboard) opened signed in by `crates/engine-cli/tests/themes.rs`, and
checked with TagCheck and axe-core by `scripts/check-a11y.sh` in CI.
