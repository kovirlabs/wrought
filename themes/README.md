# Starter themes

Complete starting points built only from the built-in components, applied
by `engine new <name> --preset <preset> --theme <theme>` on top of the
preset's example site:

| Theme | Presets | What it adds |
| --- | --- | --- |
| `docs` | static, cms, app | a documentation home page, a `docs/` section with sidebar navigation, table of contents and previous/next links |
| `business` | static, cms, app | a business home page (hero, services, about, testimonials, history, call to action) and a contact page |
| `dashboard` | cms, app | a signed-in `/dashboard/` with stat cards, a chart, a data table, alerts, tabs and a dialog |

Each theme is a folder: `theme.toml` (label, presets, design tokens, extra
navigation, extra routes) and `files/`, copied over the preset's files.
Everything is ordinary site files afterwards: edit them freely.
