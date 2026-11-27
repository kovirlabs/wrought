defmodule BenchWeb.SearchController do
  use BenchWeb, :controller

  def show(conn, params) do
    q = params |> Map.get("q", "") |> to_string() |> String.trim() |> String.slice(0, 200)
    results = if q == "", do: [], else: Bench.Blog.search(q)
    render(conn, :show, q: q, results: results, page_title: "Search")
  end
end
