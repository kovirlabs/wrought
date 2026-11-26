## v0.6 "components" (2026-11-26)

### The Tag stack covers tabs and the mobile menu: no custom element for them
- **Decision:** checked TagAction 0.2.0's source before writing scripts. It
  already implements the ARIA tabs pattern (`role="tablist"`/`tab`/
  `tabpanel`, roving `tabindex`, arrow keys, Home/End, `hidden="until-found"`
  so find-in-page and `#panel` links select a tab), disclosure dropdowns in
  the header nav (`<li><details>`: Escape, click outside, arrow keys),
  popover menus (arrow keys, close on link), and file inputs (file list,
  thumbnails, drag and drop). TagBrush 2.0.1 styles the phone menu (a
  native `popover` directly in the header's nav: links in the bar on wide
  screens, behind the button on phones) and the tab row. So `tabs`,
  `menu`, `mobile_menu` and `file_upload` emit exactly that markup and ship
  no script.
- **Mobile menu = native popover**, not a `button[aria-expanded]`
  disclosure: it needs no JavaScript at all (open, Escape, light dismiss
  and focus order are native; browsers expose the invoker's expanded
  state), browsers without popover show the links, and it is the pattern
  TagBrush lays out. A TagAction `aria-expanded` dropdown would hide the
  links on wide screens too.
- **Known limit, logged:** without JavaScript the tab buttons do nothing;
  all panels show one after another (TagAction's documented fallback), so
  no content is lost.
- **Rejected:** `<wrought-tabs>`/`<wrought-menu>` custom elements
  (duplicating TagAction, against "custom elements only for what the Tag
  stack doesn't cover"); a CSS-only `:target` tab hack (breaks history and
  the ARIA pattern).

### One component script: `video_embed`'s click-to-load player
- **Decision:** the only behaviour the Tag stack lacks is loading a
  third-party player on request. `video_embed` renders a plain `<iframe>`
  (lazy, titled, `youtube-nocookie.com` / Vimeo `dnt=1`) by default, so it
  needs no script; with `consent=true` it renders a link to the video inside
  `<wrought-video>`, whose module adds a Play button that swaps in the
  player and focuses it. No request goes to the provider before Play.
- **Rejected:** a facade by default (a link instead of a video without
  JavaScript); embedding any URL unchecked (only `https://` players and
  site-relative video files; IDs are validated).

### Component assets: content-hashed files, emitted by the template, deduplicated per page
- **Decision:** a manifest names its assets (`script = "x"`, `style = "y"`;
  files `components/x.js`, `y.css` next to the templates, embedded).
  `engine build` writes every compiled component's assets to
  `/_engine/components/<name>-<fnv32>.<ext>` with the other engine assets.
  The template prints them with `{{ component_assets("name") }}` exactly
  when the page needs them (so `video_embed` only with `consent`); after
  rendering a page the engine drops repeated identical tags
  (`components::dedup_assets`, in every render path of `Renderer`). Module
  scripts and stylesheet links, no inline code: strict CSP works.
- **Why emitted inline, not collected into `<head>`:** fragments of request
  pages are cached; a collector would have to store assets with each
  fragment and could lose a script when a cached fragment is reused on a
  page that did not otherwise need it. Inline tags travel with the markup;
  module scripts are deferred anyway and `<link rel=stylesheet>` is valid
  in the body. Dedup is cosmetic (browsers run a module once per URL).
- **Content hash, not the component version, in the name:** several
  components share one stylesheet (`layout`), and a hash changes exactly
  when the bytes change. FNV-1a, so names are stable across Rust versions.
- **Rejected:** one global component stylesheet in `base.html` (every page
  pays for it, and sites with their own `base.html` would lose it); inline
  `<style>`/`style=""` (blocked by a strict CSP; TagBrush's own docs use
  `style="--grid-min-column"`, the `grid` component uses `data-min`).

### Layout CSS only where TagBrush has nothing
- `grid` and `container` are TagBrush's `.grid` and `.container`; `divider`
  is `<hr>`; `section` uses TagBrush's bands (`data-theme`). A small
  `layout.css` adds `stack`, `columns` ratios, `spacer` sizes, the grid's
  `data-min` and the reading-width container: only TagBrush length
  properties (`--spacing-*`, `--grid-gap`, `--reading-width`), no colours,
  in `@layer wrought-components` so a site's CSS wins.

### Manifests: one TOML per component, embedded, the future block type
- **Decision:** every component (all 52, including the existing ones and
  the feature crates' Listings, Forms, Accounts and App UI) has
  `components/<name>.toml`: `label`, `version`, `group`, `features`,
  `description`, `script`/`style`, `reads` (page context it uses), typed
  `[[args]]` (types aligned with content field types plus `html`, `list`,
  `object`, `page`, `any`; `items`/`fields` for lists), `[editor]` (`icon`,
  `thumbnail`, `layout` rows, `slots`, `hidden`) and an `[example]`. A test
  checks every template has a valid manifest and every example renders.
  Sites may add `templates/components/<name>.toml` for their own
  components. `components::Catalogue` loads them for a build (the feature
  crates' folders it is given, core's last, then the site's).
- **Exposed as data:** `engine components [--json]` (no database needed:
  the CLI lists the compiled feature crates' component folders) and, with
  `admin`, `GET /admin/api/components` / `GET /admin/api/tokens` (signed-in
  users with admin rights; JSON 401/403, never a redirect; computed at
  start). Tokens come with their custom property and kind (`color`, `font`,
  `length`, `number`, `background`) for the editor's guardrails.
- `component(name, args, **kwargs)` now takes a map of arguments too (a
  block, an example); names starting with `_` are partials (`_nav.html`,
  the menu list shared by `menu`, `mobile_menu` and `header`).
- **Rejected:** manifests in Rust code (not editable by theme authors, and
  not the shape a site's own components can use); JSON Schema (heavier,
  and TOML is the project's config language).

### A block type may name a component
- **Decision:** `types/blocks/<name>.toml` takes `component = "quote"`;
  `render_block` uses `templates/blocks/<name>.html` when the site has one
  and the component otherwise, with the block's fields as arguments.
  `Registry::load` checks the component exists and, for a built-in one the
  site does not override, that every field is one of its arguments. It
  fits cleanly because blocks and component arguments are both flat field
  maps; the admin's block forms are unchanged.
- **Rejected:** deriving the block's fields from the manifest (type files
  stay the single source of content schema and migrations; the visual
  editor milestone can revisit it).

### Versioning policy
- Each component has its own SemVer version (all 1.0.0 in 0.6): patch for
  fixes keeping structure and arguments, minor for additions that keep
  existing pages the same, major for any change to elements, nesting,
  attributes, ids, arguments or defaults a site may rely on. Majors only in
  a release marked breaking (0.x: a new minor; from 1.0: a new major), each
  with an upgrade note in `docs/content/docs/components.md`, which records
  the 0.5 → 0.6 changes to `header` (now built on `mobile_menu`, visible
  "Menu" text, `aria-label="Main"`).

### Docs gallery: live core groups, generated static examples for gated groups
- **Decision:** the docs are built by the default static engine. The
  gallery renders every core component live from its manifest example
  (`component_catalogue()`); the Listings, Forms, Accounts, App UI (and
  later Commerce) groups are included from `docs/templates/gallery/<group>.html`,
  generated by `crates/engine-cli/tests/gallery.rs` with the real
  templates and extensions of an all-features build (sample data in place
  of the database, the forms' time-trap stamp scrubbed). The test fails
  when a file is stale (`WROUGHT_UPDATE_GALLERY=1` regenerates). Sample
  forms for the examples live in `docs/forms/` (unused by the static
  build).
- **Rejected:** building the docs with an app engine in CI (request-only
  components still could not render in build mode, and the docs would need
  a database); hand-written static examples (they drift from the
  templates).

### Accessibility tooling: `scripts/check-a11y.sh`
- Builds an `app` engine, copies each site into `target/a11y-work`, builds
  it (SQLite for database sites), runs TagCheck (`engine check`) on the
  output, serves it and runs axe-core 4.14 through Playwright 1.61.1
  (headless Chromium shell, installed into `target/a11y-tools`) on every
  page in light and dark schemes at 1280 and 390 px wide, against WCAG
  2.0/2.1/2.2 A and AA. The cms example's admin is crawled signed in (a
  throwaway account) and its server HTML is also given to TagCheck. Sites
  are lines in `SITES` (`name folder static|db|db+admin`), so starter
  themes are one line each.
- **small-app on SQLite** cannot run `engine serve` (it asks for mail and
  jobs, Postgres-only), so its built pages are checked and its request
  pages are covered through the same components in the gallery.

### Found by the accessibility run: browsers could not sign in (engine-auth)
- **Finding:** driving the admin with a real browser (Playwright Chromium)
  for axe, every sign-in failed with "Form expired". Engine pages sent
  `Referrer-Policy: no-referrer`; under that policy browsers send
  `Origin: null` with a form POST, and the CSRF origin check
  (`web::same_origin`) refuses a `null` origin. The end-to-end tests use an
  HTTP client that sends no `Origin`, so they never saw it.
- **Decision:** `Referrer-Policy: same-origin` on engine pages: same-origin
  posts carry their real origin, and no address (with a reset token) is
  sent to other sites. The origin check stays strict (`null` refused).
- **Rejected:** accepting `Origin: null` (that is also what sandboxed and
  some cross-site contexts send).
