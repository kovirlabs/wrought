+++
title = "The component library"
description = "Every built-in component, how components load their scripts, theming, overriding, manifests for the visual editor, and the versioning policy."
weight = 5
+++

Hammerwright ships a library of ready-made components inside the binary. Each one
is a template (`components/<name>.html`), a manifest (`components/<name>.toml`:
version, group, the feature it needs, its arguments, editor hints and an
example), and sometimes a small script or stylesheet. They render complete,
semantic HTML that works without JavaScript; TagBrush.css styles it and
TagAction adds the expected behaviour. See them all, rendered, in the
[component gallery](/components/).

```jinja
{{ component("tabs", label="Install with", items=[
     {"label": "Cargo", "markdown": "`cargo build --release`"},
     {"label": "Binary", "text": "Download a release."}]) }}
{{ component("quote", block) }}  {# a map of arguments, e.g. a block #}
```

`component(name, args, **kwargs)` takes the arguments as keywords, as a map
(a block, the manifest's example), or both (keywords win).

## The groups

Only the components of features compiled into the engine exist; the others
have no templates in the binary. `engine components` lists those of your
build.

| Group | Components | Needs |
| --- | --- | --- |
| Layout | `section`, `columns`, `grid`, `stack`, `container`, `spacer`, `divider` | core |
| Navigation | `header`, `menu`, `mobile_menu`, `breadcrumbs`, `footer`, `pagination`, `tabs`, `section_nav`, `prev_next`, `toc` | core |
| Content | `hero`, `rich_text`, `image`, `gallery`, `video_embed`, `quote`, `card`, `cards`, `accordion`, `cta`, `timeline`, `callout`, `page_list`, `post_meta` | core |
| Listings | `content_list`, `filtered_list`, `tag_cloud`, `related_items`; `search_box`, `search_results`; `pagefind_search` | `content`; `search`; core |
| Forms | `form`, `contact_form`, `newsletter_signup`, `multi_step_form`; `file_upload` | `forms`; `forms` + `media` |
| Accounts | `login_form`, `register_form`, `reset_form`, `profile_form`, `account_menu` | `auth` |
| App UI | `data_table`, `stat_card`, `chart`, `alert`, `modal`, `toast`, `empty_state` | `request` routes |
| Commerce | product card, product grid, cart, checkout summary, order history | `payments` (coming) |

Arguments are listed under each component in the [gallery](/components/)
and in `engine components --json`. The Accounts, App UI and Forms groups
are described further on [their own page](/docs/app-components/) and the
[forms page](/docs/forms/); Listings on the [search page](/docs/search/).

### Layout

Layout components take other markup: pass component output, or a block of
template text captured with `{% set %}`:

```jinja
{% set intro %}<p>Plain HTML, no classes.</p>{% endset %}
{{ component("section", id="why", label="Why", title="One engine", body=intro) }}
{{ component("columns", ratio="2-1", columns=[component("rich_text", html=page.content),
                                             component("card", title="Next", href="/docs/")]) }}
{{ component("stack", gap="lg", items=[component("hero", title="Hi"), component("cta", title="Go", label="Start", url="/x/")]) }}
```

`grid` and `container` are TagBrush's own `.grid` and `.container`;
`columns` (ratios `1-1`, `2-1`, `1-2`, `1-1-1`, `1-2-1`, `1-1-1-1`, stacked on
phones), `stack` (gaps `xs` to `xl`), `spacer` and the reading-width
container use a small stylesheet that loads only on pages using them.
`divider` is an `<hr>`.

### Navigation

- `menu`: a `<nav>` of links (default `[site] nav`); an item with
  `children` becomes a `<details>` disclosure, which in the page header's
  nav is a dropdown (TagAction closes it on Escape or a click outside and
  adds arrow keys). The current page is marked with `aria-current`.
- `mobile_menu`: a Menu button and a native popover holding the same list.
  Put it directly inside the page header's `<nav>` (the built-in `header`
  does): wide screens show the links in the bar, phones show the button.
  No JavaScript: the popover opens and closes natively (Escape and a click
  outside close it, the browser exposes the button's expanded state);
  TagAction adds arrow keys and closes it when a link is followed.
  Browsers without popover support simply show the links.
- `tabs`: the ARIA tabs pattern (`role="tablist"`, `tab`, `tabpanel`,
  `aria-selected`, `aria-controls`), which TagAction switches: arrow keys,
  Home and End move between tabs, a link to a panel or find-in-page
  selects its tab. Without JavaScript every panel shows, one after
  another. Give each tab set a unique `id` (it prefixes the element ids)
  and a `label`.

### Content

- `rich_text` prints `html` the engine made (`page.content`, a `markdown`
  field of database content, component output), or renders `markdown` an
  editor wrote (raw HTML is shown as text, links keep only http(s),
  `mailto`, site and `#` addresses; the same as the `markdown` filter), or
  splits plain `text` into paragraphs.
- `video_embed` takes a YouTube or Vimeo link (played through
  `youtube-nocookie.com` and Vimeo's `dnt=1` player), another `https://`
  player, or a `.mp4`/`.webm`/`.ogv` file (a `<video>` with controls, an
  optional captions track and a download link). `title` names the player
  for screen readers. With `consent=true` a third-party video is a link
  until the visitor presses Play: then a `<hammerwright-video>` element puts the
  player in its place. That is the only built-in component with its own
  script, and the script loads only on pages that use it this way.
- `timeline` is TagBrush's numbered steps: an `<ol>` whose items start with
  a heading, each with an optional `<time>`.

## Scripts and stylesheets load only where used

Most components need no script of their own: the Tag stack covers tabs,
dialogs, dropdowns, disclosure menus, table sorting, file inputs and the
rest. Where it does not, a component has a small, dependency-free custom
element. Its manifest names the asset (`script = "video_embed"`,
`style = "layout"`); the file sits next to the template.

- `engine build` writes every compiled component's assets to
  `/_engine/components/<name>-<hash>.js|.css` (the hash is of the content,
  so they can be cached for ever and change name when they change). They
  are served from your own origin, like TagBrush and TagAction.
- A component template asks for its assets with
  `{{ component_assets("video_embed") }}`, which prints
  `<script type="module" src="/_engine/components/video_embed-….js"></script>`
  (and a `<link rel="stylesheet">` for a style). It does so only when the
  page needs it (`video_embed` only with `consent=true`).
- A page that uses a component several times gets one copy of each tag:
  the engine drops repeats after rendering the page.
- No inline script and no inline styles, so a strict
  `Content-Security-Policy` (`script-src 'self'`, `style-src 'self'`) works.
- A site that overrides a component can keep the built-in behaviour by
  calling `component_assets` in its own template, or leave it out.

## Theming

Components use only TagBrush's elements and custom properties. Component
stylesheets contain no colours: lengths come from the spacing and width
[design tokens](/docs/styling/#design-tokens) (`--spacing-*`,
`--grid-gap`, `--reading-width`) and they sit in a cascade layer
(`hammerwright-components`), so your own CSS wins. Set colours, fonts, radius
and spacing once in `[ui.tokens]` and every component follows.

## Overriding a component

Add `templates/components/<name>.html` to your site: it replaces the
built-in one everywhere, including inside other components (the `header`
uses `mobile_menu`). Start from a copy of the built-in template (in the
source, `crates/engine-core/builtin/components/` and the
`site-templates/components/` folder of each feature crate) and change it.
`engine components` marks overridden components. A site can describe its
own components the same way, with `templates/components/<name>.toml` next
to the template (the same fields as a built-in manifest); they then show
up in `engine components` and in the admin's catalogue.

## Manifests and the visual editor

Each component is a block type plus a template plus optional behaviour.
The manifest is the block type: it declares the arguments with types
(`text`, `markdown`, `html`, `url`, `integer`, `number`, `boolean`, `date`,
`select` with `options`, `list` with `items` or `fields`, `object`,
`media`, `page`), defaults and help, and editor hints: an `icon`, a
`thumbnail`, a field `layout` (rows of argument names), `slots` (arguments
that hold other blocks) and `hidden` for frame parts such as the header.
`reads` names the page context a component uses besides its arguments
(`page`, `listing`, `request`, `user`), which tells an editor what can be
previewed out of a page.

A block type file that names a component and has no `[[fields]]` takes its
fields and editor hints from the manifest, so a new component reaches the
[visual editor](/docs/visual-editor/) with a three-line block type file.

```toml
label = "Tabs"
version = "1.0.0"
group = "navigation"
features = ["core"]
description = "Panels behind a row of tabs."

[[args]]
name = "label"
type = "text"
required = true

[editor]
icon = "tabs"
slots = ["items"]

[example]
label = "Install with"
```

The catalogue is available as data:

- `engine components` lists the components of the build (and the site's
  overrides and own components) and the site's design tokens;
  `engine components --json` prints everything, arguments and editor hints
  included.
- With the `admin` feature, signed-in admin users can fetch
  `GET /admin/api/components` and `GET /admin/api/tokens` (JSON; 401 when
  signed out, 403 without admin rights).

### A block type that is a component

A site's block type can name a component instead of having its own
template. Its fields become the component's arguments:

```toml
# types/blocks/pull_quote.toml
label = "Pull quote"
component = "quote"

[[fields]]
name = "text"
type = "text"
required = true

[[fields]]
name = "cite"
type = "text"
```

`render_block(b)` uses `templates/blocks/pull_quote.html` if the site has
one, and the component otherwise. Loading the types checks that the
component exists and, for a built-in component, that every field is one of
its arguments.

## Versioning

Every component has its own version, `MAJOR.MINOR.PATCH`, in its manifest
(shown by `engine components` and in the gallery). The rules:

- **Patch** (1.0.0 → 1.0.1): fixes that keep the markup's structure and
  every argument: a wrong attribute, an accessibility fix that adds an
  attribute, a text change.
- **Minor** (1.0 → 1.1): additions that keep existing pages the same: a new
  optional argument, a new value for a `select` argument, a new asset that
  only loads when a new argument asks for it.
- **Major** (1 → 2): anything a site's CSS, scripts, tests or overrides may
  depend on: elements, nesting, attributes or ids that change or go away,
  renamed or removed arguments, changed defaults, a script on pages that
  had none.

A major component change ships only in a release of Hammerwright that is marked
as breaking (while Hammerwright is 0.x, a new minor release such as 0.7; from
1.0 on, a new major release), and every one comes with an upgrade note
below: what changed, why, and what to change in a site that overrides or
styles the component. The components' versions start at 1.0.0 in Hammerwright
0.6; earlier releases had no component versions.

### Upgrade notes

- **0.6.0**: first versioned release of the library (every component
  1.0.0). Changes from 0.5 that affect overrides and site CSS: the
  `header`'s phone menu now comes from the `mobile_menu` component, its
  `<nav>` has `aria-label="Main"`, and its button reads "☰ Menu" (the
  symbol hidden from screen readers) instead of a bare "☰" with an
  `aria-label`; `hero` and `cta` take a `level` argument (defaults
  unchanged).

## Accessibility

Components target WCAG 2.2 AA: semantic elements with their native keyboard
behaviour, the ARIA patterns where an element does not exist (tabs,
disclosure navigation), visible focus from TagBrush, labels on every form
field, names on every navigation landmark, and targets at least 24 by 24
CSS pixels. `scripts/check-a11y.sh` in the repository builds the example
sites, these docs (with the gallery) and the admin, and checks them with
TagCheck and axe-core.
