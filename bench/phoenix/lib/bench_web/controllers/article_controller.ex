defmodule BenchWeb.ArticleController do
  use BenchWeb, :controller
  alias Bench.Blog

  def home(conn, _params) do
    render(conn, :home, articles: Blog.home(), page_title: nil)
  end

  def index(conn, params) do
    page = max(String.to_integer(Map.get(params, "page", "1")), 1)
    {articles, pages} = Blog.page(page)
    render(conn, :index, articles: articles, page: page, pages: pages, page_title: "Articles")
  end

  def show(conn, %{"slug" => slug}) do
    article = Blog.article!(slug)
    render(conn, :show, article: article, related: Blog.related(article), page_title: article.title)
  end
end
