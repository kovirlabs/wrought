# Loco (Rust) for the benchmark: the current stable Rust and Loco, a new
# application made with `loco new` (PostgreSQL, in-process background
# workers, server-side Tera views), with the bench's migration, controller,
# views and seed task copied over it. Production: a release build
# (`cargo build --release`) started with `-e production`, the generated
# config/production.yaml unchanged. See docs/content/docs/benchmarks.md.
FROM rust:1.100.0 AS builder
RUN cargo install loco --version 1.3.1 --locked
WORKDIR /srv
RUN loco new --name bench --db postgres --bg async --assets serverside -a
WORKDIR /srv/bench
RUN cargo build --release
COPY src/ src/
COPY migration/ migration/
COPY assets/ assets/
# Copied files keep their (older) modification times; touch them so cargo
# rebuilds over the first build of the generated app.
RUN find src migration -name '*.rs' -exec touch {} + \
 && cargo build --release \
 && echo "loco-rs $(grep -A1 '^name = "loco-rs"' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p'), sea-orm $(grep -A1 '^name = "sea-orm"$' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p'), $(rustc --version | cut -d' ' -f1-2)" > VERSIONS

FROM debian:trixie-20261104-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /srv/bench/target/release/bench-cli /usr/local/bin/bench-cli
COPY --from=builder /srv/bench/config config
COPY --from=builder /srv/bench/assets assets
COPY --from=builder /srv/bench/VERSIONS VERSIONS
EXPOSE 5150
CMD ["bench-cli", "start", "-e", "production"]
