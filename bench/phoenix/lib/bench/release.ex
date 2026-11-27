defmodule Bench.Release do
  @moduledoc """
  Release commands: `migrate` as `mix phx.gen.release` generates it, and
  `seed/1` for the benchmark's articles
  (`bin/bench eval 'Bench.Release.seed("/bench/data/articles-200.json")'`).
  """
  @app :bench

  def migrate do
    load_app()

    for repo <- repos() do
      {:ok, _, _} = Ecto.Migrator.with_repo(repo, &Ecto.Migrator.run(&1, :up, all: true))
    end
  end

  def rollback(repo, version) do
    load_app()
    {:ok, _, _} = Ecto.Migrator.with_repo(repo, &Ecto.Migrator.run(&1, :down, to: version))
  end

  def seed(path) do
    load_app()

    for repo <- repos() do
      {:ok, n, _} = Ecto.Migrator.with_repo(repo, fn _ -> Bench.Blog.seed(path) end)
      IO.puts("seeded #{n} articles")
    end
  end

  defp repos, do: Application.fetch_env!(@app, :ecto_repos)

  defp load_app do
    Application.ensure_all_started(:ssl)
    Application.ensure_loaded(@app)
  end
end
