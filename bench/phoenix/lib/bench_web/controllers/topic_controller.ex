defmodule BenchWeb.TopicController do
  use BenchWeb, :controller

  def show(conn, %{"slug" => slug}) do
    {topic, articles} = Bench.Blog.topic!(slug)
    render(conn, :show, topic: topic, articles: articles, page_title: topic.name)
  end
end
