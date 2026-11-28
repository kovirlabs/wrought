+++
title = "Styling and behaviour"
description = "The Tag stack, [ui] providers and design tokens."
weight = 6
+++

Hammerwright's components emit plain semantic HTML. How it looks and behaves comes
from two providers set in `site.toml`:

```toml
[ui]
css = "tagbrush"        # TagBrush.css: classless CSS for semantic tags
behaviour = "tagaction" # TagAction: expected behaviour for semantic tags
```

Both are the defaults, and both are **bundled in the engine**: a build writes
them to `public/_engine/` and pages load them from your own site, never from a
CDN. The pinned versions are TagBrush.css 2.0.1 and TagAction 0.2.0, both
Apache-2.0 licensed; their licence texts are written to
`/_engine/licenses/`.

## Swapping providers

Each provider accepts:

| Value | Effect |
| --- | --- |
| `"tagbrush"` / `"tagaction"` | The bundled Tag stack file |
| `"none"` | Load nothing |
| `"/css/site.css"` | A file from your `static/` folder |
| `"https://..."` | An external URL |

## Design tokens

Set colours, fonts, radius and spacing once in `[ui.tokens]`, and every
component follows. Hammerwright's token names map onto TagBrush's CSS custom
properties:

| Token | TagBrush property |
| --- | --- |
| `primary` | `--color-primary` (buttons, links and focus rings derive from it) |
| `background`, `surface`, `text`, `heading`, `muted`, `border` | `--color-background`, `--color-surface`, `--color-text`, `--color-heading`, `--color-text-muted`, `--color-border` |
| `link`, `on_primary`, `highlight` | `--color-link`, `--color-on-primary`, `--color-highlight` |
| `error`, `success`, `warning` | `--color-error`, `--color-success`, `--color-warning` |
| `font`, `font_heading`, `font_mono` | `--font-family-base`, `--font-family-heading`, `--font-family-mono` |
| `font_size`, `heading_weight`, `line_height` | `--font-size-base`, `--font-weight-heading`, `--line-height` |
| `radius`, `spacing` | `--border-radius`, `--spacing` |
| `width`, `reading_width` | `--container-max-width`, `--reading-width` |
| `hero_background` | `--hero-background` |

A colour can be one value, or a `[light, dark]` pair, which becomes CSS
`light-dark()` so both colour schemes stay right. Any other TagBrush property
can be set by its raw name:

```toml
[ui.tokens]
primary = "#b45309"
background = ["#fafaf9", "#0c0a09"]
font_heading = "Georgia, serif"
"--card-padding" = "2rem"
```

Tokens are written to `/_engine/theme.<hash>.css` (named by a hash of its
content, so it is cached for a year and a token change gives it a new URL),
loaded after TagBrush. It is not
inside a cascade layer, so it wins over TagBrush's defaults. Values containing
`;`, braces or `<` are rejected.

Components are themed only through these properties: the few component
stylesheets (layout components) use lengths from the spacing and width
tokens and no colours of their own, inside the `hammerwright-components` cascade
layer, so a site's own CSS always wins. See
[the component library](/docs/components/#theming).

The tokens are also available as data, each with its custom property, kind
(`color`, `font`, `length`, `number`, `background`) and the site's value:
`engine components --json` (under `design`), and with the admin
`GET /admin/api/tokens` for signed-in admin users, with the choices `token`
fields offer. The [visual editor](/docs/visual-editor/) offers only these
names (never a CSS value), so editors cannot break the design.

## Checking pages

TagCheck, the third part of the Tag stack, checks the built HTML for semantic
and accessibility mistakes. See [`engine check`](/docs/cli/#engine-check).
