+++
title = "Publishing to a bucket"
description = "Pages and media in an S3-compatible bucket behind a CDN, with invalidation of exactly the changed paths."
weight = 12
+++

By default `engine build` and the publisher write to the site's output
folder. With the `s3` feature (in the `cms` and `app` presets; a static
build adds it with `cargo build --release --features s3`) they can write to
an S3-compatible bucket instead, and a CDN serves the site from it. The
server then only handles editing, publishing and dynamic routes; a static
site needs no running server at all: `engine build` uploads it.

One S3 API covers AWS S3, Google Cloud Storage (its XML API), Cloudflare R2,
Backblaze B2 and MinIO.

```toml
[publish]
target = "s3"
bucket = "example-site"
region = "eu-west-1"
# prefix = "www"              # keys under www/ (default: the bucket root)
# endpoint = "https://..."    # any store other than AWS S3, see below
# path_style = true           # default: true when `endpoint` is set
# credentials = "auto"        # auto | env | aws-instance | gcp-metadata
# html_max_age = 60           # Cache-Control of pages, in seconds
# asset_max_age = 3600        # other files without a fingerprint

[publish.cdn]
provider = "cloudfront"       # none | log | file | cloudfront | google
distribution_id = "E2ABCDEF123456"
```

| Store | `endpoint` | `region` |
| --- | --- | --- |
| AWS S3 | (none) | the bucket's region |
| Google Cloud Storage | `https://storage.googleapis.com` | `auto` |
| Cloudflare R2 | `https://<account>.r2.cloudflarestorage.com` | `auto` |
| Backblaze B2 | `https://s3.<region>.backblazeb2.com` | e.g. `eu-central-003` |
| MinIO | `http://127.0.0.1:59000` (see below) | `us-east-1` |

## What goes where

- A page `/articles/my-post/` is the object `<prefix>articles/my-post/index.html`,
  the home page `<prefix>index.html`, other files keep their paths
  (`404.html`, `sitemap.xml`, `_engine/...`, everything from `static/`).
  The CDN maps directory URLs to `index.html`: a CloudFront Function on
  AWS, the bucket's main page suffix (`MainPageSuffix`) on Google Cloud.
- Media library files go to `<prefix>media/<id>/<name>.<ext>` when they are
  uploaded, with their resized copies, and are deleted with them. Private
  files (form attachments) never go to the bucket. The media folder on disk
  still holds every file: the admin reads private files from it and
  `engine serve` answers `/media/` from it.
- Hidden files (`.engine-manifest.json`) are not uploaded.

## `engine build`

`engine build` renders into the output folder as always, then makes the
bucket match it:

1. one listing of the prefix gives each object's ETag (for a single-part
   upload, the MD5 of its body);
2. files whose MD5 matches are left alone; the rest are uploaded;
3. objects with no file are deleted, **never under `media/`**;
4. the URL paths of everything uploaded or deleted are invalidated at the
   CDN, in one batch. New media files are not invalidated (a new upload
   always has a new number, so no CDN holds an old copy).

A second build with nothing changed uploads nothing and invalidates
nothing. The comparison is against the bucket itself, not a local record,
so it also repairs objects changed or removed by hand.

There is no render-on-miss with a bucket (the CDN never asks the server for
a missing page), so run `engine build` on first deploy and after template
changes: it pre-renders every publish route.

## Publishing (`engine serve`)

With `target = "s3"`, `engine serve` publishes through the bucket and says
so when it starts. After each change (NOTIFY, then the tag lookup, then the
re-render):

- a page whose new bytes are identical to the last published version (the
  tag index keeps its SHA-256) is **not** uploaded and not invalidated;
  the log counts it as `unchanged`. Editing an article's body re-renders
  every listing of articles, but only the article page itself changes, so
  only that object is uploaded and only its paths are invalidated;
- a changed page is uploaded, then written to the output folder too, which
  stays an exact copy of what was published;
- a page that no longer exists (unpublished, deleted, slug changed) is
  deleted from the bucket and invalidated;
- then the batch's changed paths are invalidated at the CDN.

`engine rebuild` works the same way.

## Headers

| Files | `Cache-Control` |
| --- | --- |
| HTML pages | `public, max-age=60` (`html_max_age`): a missed invalidation heals within a minute |
| Fingerprinted files: `_engine/tagbrush-2.0.1.min.css`, names with a hash such as `app.3f2a9c1d.js` | `public, max-age=31536000, immutable` |
| Media (`media/`) | `public, max-age=31536000, immutable` |
| Everything else | `public, max-age=3600` (`asset_max_age`) |

`Content-Type` comes from the file extension (`text/html; charset=utf-8`
and so on); PDFs in `media/` get `Content-Disposition: attachment`. Every
upload sends `Content-MD5`, so the store refuses a body damaged on the way,
and its ETag (the MD5) is checked. Headers a bucket cannot store,
`X-Content-Type-Options: nosniff` and the `sandbox` Content-Security-Policy
that `engine serve` sends with media, belong in the CDN's response headers
policy (CloudFront) or the backend bucket's custom response headers
(Cloud CDN).

### Pre-compressed copies in the bucket

The Brotli and gzip copies of every text file (see
[publishing](/docs/publishing/#pre-compressed-copies)) are uploaded beside
it as `<key>.br` and `<key>.gz`, with the file's `Content-Type` and
`Cache-Control` and `Content-Encoding: br` or `gzip`, for every text file
(small ones too, so a CDN function can rely on them); a publish uploads the
copies before the page, and a delete removes them. A bucket
does not choose between them by itself: the CDN's request function does,
the same function that maps directory URLs to `index.html`. On CloudFront
(viewer request; include `Accept-Encoding` in the cache key through the
cache policy's compression settings):

```js
function handler(event) {
  var r = event.request;
  if (r.uri.endsWith("/")) r.uri += "index.html";
  else if (!r.uri.split("/").pop().includes(".")) r.uri += "/index.html";
  var ae = (r.headers["accept-encoding"] || { value: "" }).value;
  if (/\.(html|css|js|svg|xml|json|txt)$/.test(r.uri)) {
    if (/\bbr\b/.test(ae)) r.uri += ".br";
    else if (/\bgzip\b/.test(ae)) r.uri += ".gz";
  }
  return r;
}
```

Without such a function the copies are simply unused (the CDN may compress
on its own, per object, at the edge).
Media files are left as they are (images and video are compressed already).

## Credentials

Keys never go in `site.toml`. A `[publish]` setting that looks like one
(`access_key_id`, `secret_access_key`, `session_token`, `token`,
`password`...) is refused with a message saying where credentials come from,
without repeating the value; so is an endpoint with a user or password in it.
Credentials are never printed, in logs or in error messages.

Where credentials may be sent is checked before every request:

- signed (SigV4) requests go over HTTPS only; plain `http://` endpoints are
  refused except on this machine (`127.0.0.1`, `::1`, `localhost`: a local
  MinIO);
- a Google bearer token (`gcp-metadata`, `GOOGLE_OAUTH_ACCESS_TOKEN`) is only
  sent over HTTPS to exactly `storage.googleapis.com` (the bucket, path-style)
  and `compute.googleapis.com` (Cloud CDN); any other endpoint must use
  SigV4 keys;
- requests that carry credentials never follow redirects.

| `credentials` | Where from |
| --- | --- |
| `auto` (default) | `env` when `AWS_ACCESS_KEY_ID` is set; otherwise `gcp-metadata` if the endpoint is Google Cloud Storage; otherwise `aws-instance` |
| `env` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional `AWS_SESSION_TOKEN` (or `GOOGLE_OAUTH_ACCESS_TOKEN`, a bearer token) |
| `aws-instance` | the machine's IAM role: the ECS task role (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `_FULL_URI`), else EC2 instance metadata (IMDSv2) |
| `gcp-metadata` | the VM's service account, a token from the GCE metadata server |

On a cloud machine, give the machine a role (AWS) or a service account
(Google Cloud) with write access to the bucket and, for invalidation,
`cloudfront:CreateInvalidation` on the distribution or
`compute.urlMaps.invalidateCache` on the URL map; nothing else is needed.
Temporary credentials are fetched again before they expire. R2, B2 and
MinIO have no machine roles: give their keys through the environment
(a systemd `EnvironmentFile`, a container secret), never the site folder.

## CDN providers

| `provider` | What it does | Settings |
| --- | --- | --- |
| `none` (default) | nothing: pages age out after `html_max_age` | |
| `log` | prints the paths it would invalidate | |
| `file` | appends each batch as one JSON line, `{"paths": [...]}`, for a script that invalidates some other way | `file` (relative to the site folder) |
| `cloudfront` | Amazon CloudFront `CreateInvalidation` | `distribution_id` |
| `google` | Google Cloud CDN `urlMaps.invalidateCache`, one call per path | `project`, `url_map`, optional `host` |

Common setting: `max_paths` (default 1000): a batch with more paths sends
one wildcard `/*` instead (a full rebuild after a template change). The
provider APIs are fixed (`https://cloudfront.amazonaws.com`,
`https://compute.googleapis.com`) and cannot be pointed elsewhere from
`site.toml`.

**Which URL forms are invalidated** (`index_paths`). A CDN caches by the
URL it was asked for, which may or may not be the object's key:

| `index_paths` | A changed `/articles/x/` invalidates | Use with |
| --- | --- | --- |
| `both` (default) | `/articles/x/` and `/articles/x/index.html` | CloudFront with a viewer-request function that rewrites to `index.html` (the safe choice when unsure) |
| `directory` | `/articles/x/` | Cloud CDN with a backend bucket: the cache key is the requested URL |
| `file` | `/articles/x/index.html` | a CDN that caches by the rewritten object key |

The home page is `/` (and `/index.html`); other files their own path
(`/sitemap.xml`).

## Costs

- **Requests:** a build lists the bucket once per 1000 objects and uploads
  only what changed; a publish uploads only pages whose bytes changed.
- **Invalidation:** CloudFront charges per path beyond a monthly free
  allowance (1000 paths at the time of writing; a wildcard counts as one
  path); `index_paths = "both"` doubles the count, so pick the form your
  CDN needs. Cloud CDN invalidations are free but rate-limited. Check your
  provider's current pricing.
- **The cheaper fallback** is `provider = "none"` with a short
  `html_max_age`: edits show within that many seconds, at no invalidation
  cost.

## Dynamic routes behind the CDN

Everything that is not a file comes from the Wrought server, as a second
origin: send `/admin/*` (previews included), `/account/*`, `/api/*`, form
posts (`/_engine/forms/*`; the rest of `/_engine/` is files in the bucket)
and `request`-mode routes such as `/search/` to it, with caching disabled;
everything else goes to the bucket. The server still
needs its output folder (the publisher mirrors pages there) and its media
folder. Caddy in front of the server is then only needed for TLS on the
server origin; if the CDN terminates TLS and reaches the server privately,
it can be left out (see [deployment](/docs/deployment/)).

## A local bucket (MinIO)

The repository has a MinIO compose file for trying this out and for the
tests (public test credentials, data in memory):

```sh
docker compose -f deploy/compose.minio.yml up -d --wait
export AWS_ACCESS_KEY_ID=wrought-test AWS_SECRET_ACCESS_KEY=wrought-test-secret
```

```toml
[publish]
target = "s3"
bucket = "my-site"
endpoint = "http://127.0.0.1:59000"

[publish.cdn]
provider = "log"
```

Create the bucket in the console at `http://127.0.0.1:59001/` (or with any
S3 tool), then `engine build`. Stop it with
`docker compose -f deploy/compose.minio.yml down -v`.

## Limits

- The bucket is written by one process per site, like the output folder.
- Uploads are single requests (no multipart), fine for pages and for media
  within `[media] max_mb`.
- An object with a non-MD5 ETag (multipart uploads, SSE-KMS encryption) is
  uploaded again on every build.
- A page whose bucket object was removed by hand is put back by the next
  `engine build`, not by the publisher (which trusts the tag index).
- `engine dev` never uploads; it builds locally.
