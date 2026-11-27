defmodule BenchWeb.TopicHTML do
  use BenchWeb, :html

  embed_templates "topic_html/*"

  def date(dt), do: Calendar.strftime(dt, "%-d %B %Y")
  def iso(dt), do: dt |> DateTime.to_date() |> Date.to_iso8601()
end
