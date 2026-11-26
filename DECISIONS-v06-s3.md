## v0.6 "s3": the bucket publish target, CDN invalidation, media on S3

### `s3` is a feature of its own, crate `engine-s3`, in the `cms` and `app` presets
- **Decision:** a new crate `engine-s3` (S3 client, SigV4, credentials,
  CDN providers, build sync, the bucket `PublishTarget`) behind the
  `engine-cli` feature `s3 = ["dep:engine-s3", "engine-core/s3"]`. It needs
  no database: a static build with `--features s3` uploads from
  `engine build` (Bloomery's "static site with no server"). `s3` joins
  `cms` (so `app`), because Bloomery's images build `--features app` and its
  bucket option must work there. The default `static` build has no HTTP
  client (`check-features.sh` now also fails on `reqwest`/`engine-s3` in
  the default tree).
- **Cost:** reqwest + hyper-util + rustls (ring, webpki roots); hyper and
  tokio were already in. About 1.5 MB on the cms/app binary.
- **Rejected:** S3 only in `app` (the cms preset is the publishing one);
  S3 inside engine-publish (static sites could not use it without the
  database crates).

### The `PublishTarget` trait moved to engine-core
- **Decision:** `engine_core::target` (behind a non-marker feature
  `targets`, switched on by `publish` and `s3`) holds `PublishTarget`,
  `DiskTarget`, `check_rel` and `TargetKind`/`target_kind()`;
  engine-publish re-exports them, so its API is unchanged. The trait gained
  three defaulted methods: `skip_unchanged()`, `is_current(rel, body)` and
  `render_on_miss()`. `PublishConfig` now only reads `target` and refuses
  bucket settings with `target = "disk"`; engine-s3 parses the rest.
- **Why:** engine-s3 implements the trait without depending on
  engine-publish (sqlx); `targets` is not a marker, so engine-cli's
  dev-dependency on engine-s3 (for the end-to-end test) does not switch on
  the `s3` marker in a default test build.

### SigV4 by hand (hmac + sha2), not the AWS SDK
- **Decision:** `engine_s3::sigv4` is ~100 lines, sans-IO, checked against
  the AWS test suite's `get-vanilla` vector and the S3 documentation's GET
  Object example, and for real by MinIO in the tests. HTTP is the workspace
  `reqwest` (rustls/ring). XML answers (ListObjectsV2, errors) are read by a
  30-line extractor; we never parse arbitrary XML.
- **Rejected:** `aws-sdk-s3` (hundreds of crates, aws-lc by default, a
  compile-time cost bigger than the rest of the engine, and no use for GCS
  bearer tokens or Cloud CDN); `rusty-s3` (fine, but SigV4 is small and we
  also sign CloudFront requests); `quick-xml` (one more dependency for two
  fixed document shapes).

### Credentials: environment, then machine roles; never site.toml
- **Decision:** `[publish] credentials = "auto" | "env" | "aws-instance" |
  "gcp-metadata"`. `env`: `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/
  `AWS_SESSION_TOKEN` (or `GOOGLE_OAUTH_ACCESS_TOKEN`). `aws-instance`: ECS
  container credentials if their variables are set, else EC2 IMDSv2
  (token first). `gcp-metadata`: the GCE metadata server's service account
  token, sent as `Authorization: Bearer` to GCS's XML API (its S3 API takes
  OAuth tokens, so GCP needs no HMAC keys at all) and to Cloud CDN. `auto`:
  env if set, else GCP when the endpoint is storage.googleapis.com, else the
  AWS role. Temporary credentials are cached until five minutes before
  expiry. The metadata addresses are fixed (see the security fix below:
  no environment overrides).
- **Safety:** `Secret` prints `<redacted>` in Debug/Display; settings whose
  names look like keys (`secret`, `access_key`, `token`, `password`,
  `hmac`, `key`...) are refused with a message naming the environment
  variables, without echoing the value; `credentials` with an unknown value
  is refused without echoing it (it may be a pasted key); endpoints with a
  user, password, path or query are refused likewise.
- **Rejected:** EKS web identity (`AssumeRoleWithWebIdentity` needs STS
  calls and token files; IMDS/ECS cover Bloomery's VMs); AWS profiles and
  `~/.aws/credentials` (stored keys on disk, which the Bloomery spec rules
  out).

### `engine build`: compare with the bucket's ETags, not a manifest
- **Decision:** render as before into the output folder, then one
  ListObjectsV2 of the prefix; a file whose MD5 equals the object's ETag is
  skipped, others are uploaded (8 at a time, `Content-MD5` sent, the
  returned ETag checked), objects with no file are deleted except under
  `media/`, then the URL paths of everything uploaded or deleted go to the
  CDN in one batch (new media excluded: new ids are never cached; new pages
  included: a CDN may hold a cached 404).
- **Why:** the bucket is the truth; a build repairs objects changed or
  removed by hand, and nothing has to be kept in step with it.
- **Rejected:** a manifest object in the bucket (drifts when the bucket is
  edited; a second source of truth); per-object HEAD (one request per file).
- **Known cost:** objects with non-MD5 ETags (multipart, SSE-KMS) are
  uploaded on every build. Header changes alone (a new `html_max_age`) are
  not detected; documented.

### Publish mode: skip pages whose bytes did not change
- **Decision:** with a target whose `skip_unchanged()` is true (the bucket),
  the publisher compares the new SHA-256 with `engine_pages.sha256` (new
  statement `PAGE_SHA256`) for publish routes, and asks the target
  (`is_current`, the mirror file) for `build` routes; equal pages are not
  written, not invalidated, and reported in a new `PublishReport.unchanged`
  list. Their tags are still recorded. The disk target keeps writing every
  re-rendered page (unchanged behaviour, and the disk tests rely on it).
- **Effect:** editing an article's body re-renders the listings (tag
  `c:article`) but uploads and invalidates only the article page (checked
  end to end).
- **The output folder stays a mirror:** the bucket target writes the bucket
  first, then the local file, so `engine serve` can still serve the origin,
  `is_current` has something to compare with, and `index_build` hashes
  the same bytes that were uploaded.
- **No render-on-miss** with a bucket (`render_on_miss() = false`; serve
  does not install the miss handler and prints so at start).

### CDN invalidation behind a trait; CloudFront and Cloud CDN both implemented
- **Decision:** `Cdn` trait with `none`, `log`, `file` (JSON line per batch:
  for scripts, and the recording double of the end-to-end test),
  `cloudfront` (CreateInvalidation, SigV4 `cloudfront`/`us-east-1`) and
  `google` (`urlMaps.invalidateCache`, one call per path, bearer token).
  An in-memory `RecordingCdn` serves the crate tests. `Invalidator`
  expands paths per `index_paths` (`both` default, `directory`, `file`)
  and sends one `/*` above `max_paths` (default 1000).
- **Why both:** each is ~60 lines once SigV4 and the token sources exist;
  Bloomery's AWS row uses CloudFront and its GCP row (the one written)
  Cloud CDN.
- **Tests:** against a local axum mock server (endpoint override): request
  shape, recomputed signature, IMDSv2 flow with session token, metadata
  token flow. Never a real cloud endpoint. (The mock is reached through
  test-only constructors, see the security fix below.)
- **`index_paths` default `both`:** whether a CDN's cache key is the
  directory URL or the rewritten `index.html` key depends on how the
  rewrite is done; invalidating both is always correct and the doubled
  path count is documented as a cost.

### Media on S3: disk stays the working copy, public files are copied
- **Decision:** engine-media had no storage abstraction; it gains a minimal
  one, `PublicStorage` (`put`, `delete`), default none. The disk folder
  keeps every file (admin downloads of private files, `/media/` from
  `engine serve`, variants); public files and their copies are also put to
  `<prefix>media/<id>/...` on upload (before the database commit; a failure
  removes what was copied and fails the upload) and deleted with the file
  (and invalidated). Private files never reach the bucket, which a CDN
  serves publicly. `engine build` also uploads public media missing from the
  bucket (for sites switching to S3), and never deletes under `media/`.
- **Rejected:** moving all media storage to the bucket (private files would
  need a second, non-public bucket, and the admin and `/media/` route would
  need an S3 read path; a larger change than v0.6 needs).

### Test infrastructure: MinIO from the Pigsty community image
- **Decision:** `deploy/compose.minio.yml` (`name: wrought-test-minio`,
  container `wrought-test-minio`, 127.0.0.1:59000, console 59001, data on
  tmpfs, public test credentials), image `pgsty/minio:RELEASE.2026-08-04T00-00-00Z`.
  MinIO Inc. no longer publishes images (`minio/minio` is gone from Docker
  Hub, quay.io needs a login); the Pigsty build is the maintained community
  one. Tests read `ENGINE_TEST_S3_ENDPOINT` (and `_ACCESS_KEY_ID`,
  `_SECRET_ACCESS_KEY`) and skip without it; each creates and destroys its
  own bucket. `scripts/test-postgres.sh` starts MinIO and passes the
  variables; `scripts/test-sqlite.sh` passes them when MinIO answers (it
  stays container-free otherwise).
- **Rejected:** a separate `scripts/test-s3.sh` (one more command to
  remember; the existing scripts already know about containers).

### Security fix: where credentials may be sent
- **Finding (review, two MEDIUM):** the Google bearer token (a machine
  credential) was sent to whatever URL was configured: the bucket
  `endpoint` (`Auth::Bearer` in `client.rs`) and `[publish.cdn] endpoint`
  (`invalidateCache` in `cdn.rs`). A mistyped or hostile endpoint, or plain
  `http://`, would leak the token. SigV4 requests could likewise go over
  plain HTTP to any host, the CDN endpoint override was settable from
  `site.toml`, the metadata addresses from environment variables, and the
  HTTP clients followed redirects. `is_google()` matched by suffix.
- **Fix:** a new module `engine_s3::endpoint` checks every credential-
  carrying request before it is sent:
  - a bearer token only over HTTPS, default port, to a host *exactly* on an
    allow-list (`storage.googleapis.com` for the bucket,
    `compute.googleapis.com` for Cloud CDN); URLs are parsed (`url`), never
    matched by substring or suffix; URLs with user info are refused;
    anything else must use SigV4;
  - SigV4 only over HTTPS, or plain HTTP to loopback (127.0.0.0/8, `::1`,
    `localhost`) for a local MinIO; checked when `site.toml` is read and
    again per request;
  - the CloudFront API only at `https://cloudfront.amazonaws.com`;
  - `[publish.cdn] endpoint` is gone; the provider tests use
    `CloudFront::with_endpoint_for_tests` / `GoogleCdn::with_endpoint_for_tests`
    and `Credentials::with_metadata_for_tests`, which accept loopback URLs
    only and are not reachable from configuration. The metadata addresses
    (169.254.169.254, `metadata.google.internal`) are fixed; the
    `AWS_EC2_METADATA_SERVICE_ENDPOINT` / `GCE_METADATA_HOST` overrides were
    removed (`AWS_CONTAINER_CREDENTIALS_FULL_URI` stays, with the SDKs'
    HTTPS-or-local check, because ECS sets it);
  - every client that carries credentials (bucket, CDN, metadata) uses
    `redirect::Policy::none()`: a 3xx is an error, never followed;
  - `is_google()` compares the whole endpoint with
    `https://storage.googleapis.com`.
- **Tests:** a bearer token over plain HTTP, look-alike hosts
  (`storage.googleapis.com.evil.test`, another port), user info, non-loopback
  plain-HTTP SigV4 endpoints, test overrides to non-loopback hosts, and a
  307 redirect from the bucket and from the CloudFront API (the redirect
  target receives nothing) are all refused.
- **Why this shape:** the allow-lists are tiny and fixed, so exact matching
  costs nothing; refusing at the last moment before `send` covers every
  path to the network, including ones added later.
- **Rejected:** a site.toml escape hatch (`allow_insecure_endpoint`) for
  private S3 gateways over HTTP (a setting a deployment could flip by
  mistake; put TLS in front of such a gateway instead).
