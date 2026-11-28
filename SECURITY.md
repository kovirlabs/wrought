# Security policy

## Supported versions

Hammerwright is pre-1.0. Security fixes go into the latest release only; there
are no backports to earlier 0.x lines. Upgrade to the newest 0.x release to
receive fixes.

| Version | Supported |
| --- | --- |
| Latest 0.x release | Yes |
| Older 0.x releases | No |
| Unreleased `main` | Fixed as part of normal development |

From 1.0 on, the latest minor release of the current major version will be
supported, and this table will say for how long the previous major gets
security fixes.

## Reporting a vulnerability

**Please do not report security problems in public issues, pull requests,
chat rooms or the Fediverse.**

Send a private report by email to:

    <SECURITY CONTACT ADDRESS — OWNER TO FILL IN>

Include as much as you can:

- the Hammerwright version or commit, and the features it was built with
  (`static`, `cms`, `app`, or the feature list);
- the PostgreSQL version if it matters;
- what an attacker can do, and what they need first (an account, a role,
  local access, a crafted upload ...);
- steps to reproduce, ideally a minimal site folder or a request sequence;
- whether you would like to be credited, and under which name.

If you prefer encrypted email, ask for a key in a first message without
details.

## What happens next

- We acknowledge your report within **5 working days**.
- We confirm or rule out the problem and tell you our assessment and a
  planned fix date, normally within **14 days**.
- We prepare the fix in private, then publish a release, an advisory in the
  release notes and `CHANGELOG.md`, and (for crates on crates.io) a
  [RustSec](https://rustsec.org/) advisory.
- We aim to publish within **90 days** of the report, sooner for problems
  that are easy to exploit. If a fix needs longer we agree a date with you.
- Reporters are credited unless they ask not to be.

Hammerwright is a small project led by one maintainer (see
[GOVERNANCE.md](GOVERNANCE.md)); these are targets, not a paid service
level.

## Scope

In scope: the `engine` binary and every crate in this repository, the
bundled templates and components, the example sites as shipped, the release
binaries and their signatures, and the deployment files in `deploy/`.

Out of scope: problems that need a malicious site developer (templates,
`site.toml` and Markdown files are trusted code, like any static site
generator's); vulnerabilities in a dependency that do not affect Hammerwright
(report those upstream; we track RustSec advisories with `cargo audit` and
`cargo deny` in CI); denial of service by sheer request volume, which belongs
to the front web server or CDN.

The security baseline Hammerwright aims for (auto-escaping, CSRF on every form,
`__Host-` cookies, Argon2id, rate limits, features not compiled in have no
routes) is described in the
[documentation](docs/content/docs/architecture.md#security) and in
[DECISIONS.md](DECISIONS.md). A way around any of it is a vulnerability.

## Verifying release binaries

Release binaries come with SHA-256 checksums and SSH signatures. How to
check them is described in
[docs/content/docs/releases.md](docs/content/docs/releases.md).
