defmodule BenchWeb.Router do
  use BenchWeb, :router

  # The generated browser pipeline, unchanged.
  pipeline :browser do
    plug :accepts, ["html"]
    plug :fetch_session
    plug :fetch_live_flash
    plug :put_root_layout, html: {BenchWeb.Layouts, :root}
    plug :protect_from_forgery
    plug :put_secure_browser_headers
  end

  pipeline :api do
    plug :accepts, ["json"]
  end

  scope "/", BenchWeb do
    pipe_through :browser

    get "/", ArticleController, :home
    get "/articles/", ArticleController, :index
    get "/articles/page/:page/", ArticleController, :index
    get "/articles/:slug/", ArticleController, :show
    get "/topics/:slug/", TopicController, :show
    get "/search/", SearchController, :show
  end

  scope "/", BenchWeb do
    pipe_through :api

    get "/up", ApiController, :up
    patch "/api/articles/:slug", ApiController, :update
  end
end
