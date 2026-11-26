+++
title = "The visual editor"
description = "Editing block fields visually: the block library, rich text, reordering, design-token choices, the live preview rendered by the engine, saving with the version check, and how block types and components feed it."
weight = 7
+++

With the `visual-editor` feature (in the `cms` and `app` presets), every
item with a `blocks` field gets **Open in the visual editor** on its admin
page (`/admin/editor/<type>/<id>/`). Editors arrange and fill blocks next to
a live preview of the page; developers decide which blocks exist. The editor
never owns the HTML: it saves the block tree, and Wrought renders it with
the site's own templates, so the preview is exactly what is published.

## What editors do

- **Add a block** from the library: the block types the field allows, with
  their label, description, icon or thumbnail. It goes after the selected
  block.
- **Edit its fields** in the side panel. `markdown` fields are rich text
  (bold, italic, code, headings, lists, quotes, links); text the rich-text
  editor could not keep (raw HTML, tables, images) opens as Markdown text
  instead, and any field can be switched to Markdown text.
- **Rearrange**: Move up / Move down buttons, Alt+↑ / Alt+↓ on the selected
  block, dragging in the list, or dragging and the move buttons on the
  selected block in the preview. Copy, remove, and Undo for each of these.
- **Click a block in the preview** to select it for editing.
- **Choose design options** (`token` fields) from the site's design tokens,
  shown as swatches with the site's real values.
- **Save** (a draft stays a draft; a published item's pages are rewritten),
  or **Save and publish** for those whose role may publish.

Everything is reachable with the keyboard; moves and saves are announced to
screen readers. Without JavaScript the page points to the ordinary edit
form, which edits the same blocks.

## Conflicts

Every save carries the version the editor started from. If someone else saved
the item in the meantime, nothing is overwritten: a dialog says so, keeps the
editor's changes, links the history, and offers to keep editing, load the
other version (discarding the editor's), or save the editor's version over it
after review.

## The preview

Each change is posted (after a short pause) to
`POST /admin/api/items/<type>/<id>/preview/`. The blocks go through exactly
the checks of a save (block types, fields, required values, URL schemes,
token names, ids) and the page through exactly the rendering of a published
page (Markdown sanitised, every other value escaped). Nothing is saved,
cached or written to disk, and the engine fetches nothing on the preview's
behalf. The page is shown in a same-origin frame
(`/admin/editor/<type>/<id>/frame/`) with its own Content-Security-Policy:
framed by the admin only, scripts from the site's origin only, forms
cannot submit.

In the preview, `render_block` wraps each block in
`<!--wrought-block:ID-->` comments so the editor knows which part of the page
a block made; published pages never have them. Blocks a template draws
without `render_block` still edit fine; they just cannot be clicked in the
preview.

## Block types for the editor

A block type is a type file (fields), a template (markup) and optional
editor hints. Nothing in the editor names a block type: add a block type
file, or a component, and it appears in the library.

```toml
# types/blocks/panel.toml
label = "Panel"
description = "A highlighted box on a background from the site's colours."

[editor]
icon = "panel"                     # or thumbnail = "/img/panel.png"
layout = [["title"], ["text"], ["background", "padding"]]

[[fields]]
name = "title"
type = "text"

[[fields]]
name = "text"
type = "markdown"
required = true

[[fields]]
name = "background"
type = "token"
token = "background"
default = "surface"
```

A block can be drawn by a component (`component = "rich_text"`). Without
`[[fields]]`, its fields come from the component's manifest: every argument a
block field can hold (`text`, `markdown`, `url`, `integer`, `boolean`,
`date`, `select`, `media`), with labels, help, options and defaults. Editor
hints come from the block file, then the manifest.

```toml
# types/blocks/text.toml
label = "Text"
component = "rich_text"
```

## Design tokens, not values

A `token` field holds the *name* of a design token from one group, never a
CSS value. Groups and names:

| `token =` | Names | Template prints | Sets |
| --- | --- | --- | --- |
| `color` | text, heading, muted, primary, link, error, success, warning | `data-color` | `color` |
| `background` | background, surface, highlight, primary | `data-background` | `background-color` (text and links on `primary` use `on_primary`) |
| `font` | font, font_heading, font_mono | `data-font` | `font-family` |
| `spacing` | xs, sm, md, lg, xl | `data-padding` | `padding` |

`options = [...]` narrows a field to some names. The template prints the
name in the group's attribute:

```html
<section data-background="{{ block.background }}" data-padding="{{ block.padding }}">
```

and the engine's token stylesheet maps it onto the site's tokens
(`[ui.tokens]` in `site.toml`). `render_block` adds the stylesheet for blocks
with token fields; elsewhere print `{{ token_assets() }}`. Any other value is
refused when saving, by the admin form, the JSON API and the editor alike.

## Block ids

Every block has a stable `id` in the block tree (`{"type": "quote", "id":
"b3", ...}`): 1 to 40 letters, digits, `_` or `-`. The store keeps valid,
unique ids and gives the others (new blocks, copies, trees saved before 0.7)
the first free `b<n>`. Templates may use `block.id`; a component drawing a
block never receives it as its own `id` argument.

## The bundle

The editor is TypeScript: Svelte 5 for the interface and Tiptap 3
(ProseMirror) for rich text, in `editor/`. `scripts/build-editor.sh` builds it
into one module and one stylesheet in `crates/engine-admin/editor-dist/`,
which is committed and embedded in the binary: building or running the
engine never needs Node. CI and releases rebuild it from the locked packages
and fail if the committed bundle differs. The licences of the bundled
packages are served at `/_engine/ui/editor-licenses-<hash>.txt`.

To work on the editor: `cd editor && npm ci --ignore-scripts`, change
`src/`, then `scripts/build-editor.sh` and commit `editor-dist/` with the
sources. `scripts/check-editor.sh` drives the whole flow in headless
Chromium.

## API

| Route | Does |
| --- | --- |
| `GET /admin/api/items/<type>/<id>/` | Stored fields, version, status, block fields with allowed types, `block_types` (fields and merged editor hints), `can_publish`, `url` |
| `POST /admin/api/items/<type>/<id>/preview/` | `{"tab", "fields": {...}}`: validate and render without saving |
| `PUT /admin/api/items/<type>/<id>/blocks/` | `{"version", "fields": {...}, "status"?}`: save every block field in one revision; 409 with `current_version` on a stale version |
| `GET /admin/api/tokens` | Design tokens and the `token` field choices |

All go through the admin gate (session, admin access, two-step sign-in when
required); writes need the session's CSRF token in `X-CSRF-Token`, and each
field touched needs write access for the user's roles.

## Not yet

- Editing text directly inside the preview: text is edited in the side
  panel (rich text for Markdown fields).
- Picking media visually: a `media` field takes the file's number, with a
  link to the media library.
- TagCheck inside the editor: run `engine check` (or `scripts/check-a11y.sh`)
  on the built site.
