+++
title = "About"
description = "What this example shows."
+++

This page is a Markdown file in `content/`, like in a static site. It uses
the `publish` render mode (the site's default), so it is part of the tag
index; it reads no database content, so content edits never rewrite it.

The [articles](/articles/), [topics](/topics/) and author pages come from
the database. The [colophon](/colophon/) is a `build` route: only
`engine build` writes it.
