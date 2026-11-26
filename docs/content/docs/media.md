+++
title = "Media"
description = "Uploads checked by their content, metadata removed, responsive image copies, and the image and gallery components."
weight = 16
+++

The `media` feature (in the `cms` and `app` presets) adds a media library:
uploads in the admin (**Media**), `engine media add` on the command line,
file fields in [forms](/docs/forms/#file-uploads), and the `media()` template
function with the `image` and `gallery` components.

```toml
[media]
dir = "data/media"     # relative to the site folder; keep it out of git
max_mb = 10            # largest upload
max_megapixels = 40    # largest image (decompression bombs are refused)
widths = [320, 640, 960, 1280, 1920]   # resized copies for srcset
jpeg_quality = 82
```

## What happens to an upload

- **The content decides the type**, not the file name or the browser's
  `Content-Type`: JPEG, PNG, GIF, WebP and PDF are accepted, by their first
  bytes. SVG, HTML and XML are refused with a message (an SVG can carry
  script), and so is everything else.
- **Size limits** apply while the upload is read (413 above `max_mb`), and
  the pixel limit is checked from the image header before any pixels are
  decoded.
- **The engine makes the file name**: `<id>/<words>.<ext>`, the words taken
  from the uploaded name (lowercase letters, digits, `-`), the extension from
  the detected type. `../../etc/Photo.php.JPG` becomes `photo-php.jpg`. No part
  of a stored path comes from the request.
- **Metadata is removed**: JPEG, PNG and WebP are decoded and written again,
  which drops EXIF (GPS position, camera serial numbers), XMP and comments,
  after applying the EXIF rotation. GIFs keep their bytes (re-encoding would
  lose animation; GIF has no EXIF). PDFs are stored as uploaded and always
  downloaded, never shown in the browser.
- **Resized copies** for each of `widths` smaller than the original are
  written next to it at upload time (GIF copies are PNG).

## How files are served

Public files are at `/media/<id>/<name>.<ext>` and their copies at
`/media/<id>/<name>-<width>.<ext>`, answered by `engine serve` (or by Caddy,
see [deployment](/docs/deployment/#media)). Every response has
`X-Content-Type-Options: nosniff` and a `Content-Security-Policy` with
`sandbox`, so even a file a browser decided to render as a page could not run
script on the site; PDFs come with `Content-Disposition: attachment`. Files
never change after upload (a new upload gets a new number), so they are
cached for a year.

Form attachments are **private**: they live in a separate folder, have no
public URL, and are downloaded from the admin by people who may read
submissions.

With the bucket publish target (`[publish] target = "s3"`), public files and
their copies are also uploaded to the bucket under `media/` when they are
added, and removed from it when they are deleted, so the CDN serves
`/media/`; private files stay on disk only. See
[publishing to a bucket](/docs/buckets/#what-goes-where).

## In templates

`media(id)` returns the file (or nothing for a missing or private one):
`url`, `srcset`, `width`, `height`, `alt`, `mime`, `variants`. A content
type can have a `media` field (a library number, chosen from a list in the
admin):

```toml
[[fields]]
name = "cover"
type = "media"
```

```jinja
{% set cover = media(page.cover) %}
{% if cover %}{{ component("image", image=cover, sizes="(min-width: 60rem) 56rem, 100vw") }}{% endif %}
{{ component("gallery", images=[media(3), media(4), media(5)]) }}
```

`image` writes one `<img>` with `srcset`, `sizes`, `width`, `height`,
`loading="lazy"` and `decoding="async"` (in a `<figure>` with a `caption`);
`alt` defaults to the file's description, and `alt=""` marks decoration.
`gallery` is a grid of linked figures. Both also take plain `src`/`srcset`
values, so they work on static sites without the media feature.

Pages record the tag `i:media:<id>`: changing a file's description or
deleting it rewrites the published pages that show it.

## Command line

```sh
engine media add --site . photo.jpg --alt "The forge at dusk"   # prints its number
engine media list --site .
```
