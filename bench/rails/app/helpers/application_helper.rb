module ApplicationHelper
  # Configuration (b), "with Rails caching": fragment caching through the
  # standard `cache` helper (Russian-doll keys from the records' updated_at)
  # when BENCH_CACHING=1; configuration (a) renders everything each time.
  def bench_cache(key, &block)
    if ENV["BENCH_CACHING"] == "1"
      cache(key, &block)
    else
      # Like `cache` on a miss: the block writes into the page.
      block.call
      nil
    end
  end
end
