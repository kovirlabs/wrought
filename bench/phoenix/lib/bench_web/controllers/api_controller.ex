defmodule BenchWeb.ApiController do
  # The edit used by the "edit to published" test: a token-protected JSON
  # update, as the Rails app has.
  use BenchWeb, :controller

  def up(conn, _params), do: text(conn, "ok")

  def update(conn, %{"slug" => slug, "title" => title}) do
    expected = System.fetch_env!("BENCH_API_TOKEN")
    token = conn |> get_req_header("authorization") |> List.first("") |> String.replace_prefix("Bearer ", "")

    if Plug.Crypto.secure_compare(token, expected) do
      {:ok, a} = Bench.Blog.update_title(slug, title)
      json(conn, %{slug: a.slug, title: a.title})
    else
      conn |> send_resp(401, "") |> halt()
    end
  end
end
