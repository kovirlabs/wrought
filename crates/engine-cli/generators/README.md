The files `engine generate` writes, embedded in the binary
(`src/generate.rs`). `__name__`-style tokens are replaced when a file is
generated; everything else is copied as it is (Jinja syntax in the HTML
templates is left alone). The integration test `tests/generate.rs` runs
every generator and builds, checks and compiles the result.
