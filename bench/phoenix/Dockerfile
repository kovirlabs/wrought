# Phoenix for the benchmark: the current stable Elixir, Erlang/OTP and
# Phoenix, a new application made with `mix phx.new` (PostgreSQL; no asset
# pipeline, LiveView pages, dashboard, mailer or gettext, which the benchmark
# does not use), with the bench's schemas, controllers, templates and seed
# function copied over it. Production: MIX_ENV=prod, an OTP release made as
# Phoenix's "Deploying with Releases" guide describes (`mix phx.gen.release`,
# `mix phx.digest`, `mix release`), Bandit, the generated config otherwise.
# See docs/content/docs/benchmarks.md.
FROM elixir:1.20.5-otp-29 AS builder
ENV MIX_ENV=prod
RUN mix local.hex --force && mix local.rebar --force \
 && mix archive.install hex phx_new 1.8.17 --force
WORKDIR /srv
RUN mix phx.new bench --database postgres --no-assets --no-live --no-dashboard \
      --no-mailer --no-gettext --no-install
WORKDIR /srv/bench
RUN mix deps.get --only prod && mix deps.compile
RUN mix phx.gen.release
COPY lib/ lib/
COPY priv/ priv/
RUN mix compile && mix phx.digest && mix release
RUN v() { sed -n "s/.*\"$1\": {:hex, :$1, \"\([^\"]*\)\".*/\1/p" mix.lock; } \
 && echo "phoenix $(v phoenix), bandit $(v bandit), ecto_sql $(v ecto_sql), elixir $(elixir -e 'IO.write System.version'), OTP $(erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),halt().')" \
    > _build/prod/rel/bench/VERSIONS

FROM debian:trixie-20261104-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libstdc++6 openssl libncurses6 locales ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen && locale-gen
ENV LANG=en_US.UTF-8 LANGUAGE=en_US:en LC_ALL=en_US.UTF-8 MIX_ENV=prod
WORKDIR /app
COPY --from=builder /srv/bench/_build/prod/rel/bench ./
EXPOSE 4000
CMD ["/app/bin/server"]
