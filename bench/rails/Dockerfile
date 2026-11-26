# Ruby on Rails for the benchmark: the current stable Ruby and Rails, a new
# application made with `rails new` (PostgreSQL, the generator's defaults
# otherwise, minus JavaScript, Action Mailbox/Text/Cable, Active Storage and
# the deployment extras the benchmark does not use), with the bench's
# models, controllers, views and seed task copied over it. Production mode,
# Puma with Rails' default config/puma.rb. See docs/content/docs/benchmarks.md.
FROM ruby:4.0.10-slim-trixie
RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends build-essential libpq-dev libyaml-dev git curl \
 && rm -rf /var/lib/apt/lists/*
RUN gem install rails -v 8.1.4.1 --no-document
WORKDIR /srv
RUN rails new bench --database=postgresql --skip-git --skip-docker --skip-kamal \
      --skip-thruster --skip-ci --skip-rubocop --skip-brakeman --skip-test --skip-system-test \
      --skip-action-mailer --skip-action-mailbox --skip-action-text --skip-active-storage \
      --skip-action-cable --skip-javascript --skip-hotwire --skip-jbuilder --skip-dev-gems
WORKDIR /srv/bench
COPY app/ app/
COPY config/routes.rb config/routes.rb
COPY config/database.yml config/database.yml
COPY config/initializers/bench.rb config/initializers/bench.rb
COPY db/migrate/ db/migrate/
COPY lib/tasks/ lib/tasks/
COPY public/ public/
ENV RAILS_ENV=production RAILS_LOG_LEVEL=info
RUN SECRET_KEY_BASE_DUMMY=1 bin/rails assets:precompile
EXPOSE 3000
CMD ["bin/rails", "server", "-b", "0.0.0.0", "-p", "3000"]
