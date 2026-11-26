class ApplicationController < ActionController::Base
  # Configuration (b) also answers conditional GETs (ETag / Last-Modified),
  # the standard HTTP caching of Rails; it only helps clients that send
  # If-None-Match, which the load generator does not.
  def http_cache(record_or_scope)
    fresh_when(record_or_scope, public: true) if ENV["BENCH_CACHING"] == "1"
  end
end
