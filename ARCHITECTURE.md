# Architecture

The architecture guide is part of the documentation site, so it is built and
checked with the rest of the docs:
[docs/content/docs/architecture.md](docs/content/docs/architecture.md)
(served at `/docs/architecture/` when you run
`cargo run -p engine-cli -- dev --site docs`).

It covers the crates, the render dial, the request flow, the data layer,
the publish pipeline, how features are wired and the extension hooks. The
reasons behind each design choice are in [DECISIONS.md](DECISIONS.md); the
design itself is in the [specification](wrought/).
