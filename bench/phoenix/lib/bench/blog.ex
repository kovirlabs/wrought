defmodule Bench.Blog do
  @moduledoc "The benchmark's content: articles, their author and topics."
  import Ecto.Query
  alias Bench.Repo

  defmodule Author do
    use Ecto.Schema

    schema "authors" do
      field :slug, :string
      field :name, :string
      timestamps(type: :utc_datetime)
    end
  end

  defmodule Topic do
    use Ecto.Schema

    schema "topics" do
      field :slug, :string
      field :name, :string
      timestamps(type: :utc_datetime)
    end
  end

  defmodule Article do
    use Ecto.Schema
    import Ecto.Changeset

    schema "articles" do
      field :slug, :string
      field :title, :string
      field :summary, :string
      field :body_html, :string
      field :published_at, :utc_datetime
      belongs_to :author, Bench.Blog.Author
      many_to_many :topics, Bench.Blog.Topic, join_through: "article_topics"
      timestamps(type: :utc_datetime)
    end

    def title_changeset(article, attrs) do
      article |> cast(attrs, [:title]) |> validate_required([:title])
    end
  end

  @per_page 10

  defp newest(q), do: order_by(q, [a], desc: a.published_at, desc: a.id)

  def home, do: Article |> newest() |> limit(3) |> preload(:author) |> Repo.all()

  def page(n) do
    total = Repo.aggregate(Article, :count)
    pages = div(total + @per_page - 1, @per_page)

    articles =
      Article
      |> newest()
      |> offset(^((n - 1) * @per_page))
      |> limit(@per_page)
      |> preload(:author)
      |> Repo.all()

    {articles, pages}
  end

  def article!(slug),
    do: Article |> where(slug: ^slug) |> preload([:author, :topics]) |> Repo.one!()

  # Up to three articles sharing the most topics with this one (the same
  # rule as Wrought's related_items and the Rails app).
  def related(%Article{id: id, topics: topics}, n \\ 3) do
    topic_ids = Enum.map(topics, & &1.id)

    ids =
      from(at in "article_topics",
        where: at.topic_id in ^topic_ids and at.article_id != ^id,
        group_by: at.article_id,
        order_by: [desc: count(), desc: at.article_id],
        limit: ^n,
        select: at.article_id
      )
      |> Repo.all()

    by_id = Article |> where([a], a.id in ^ids) |> Repo.all() |> Map.new(&{&1.id, &1})
    ids |> Enum.map(&by_id[&1]) |> Enum.reject(&is_nil/1)
  end

  def topic!(slug) do
    topic = Topic |> where(slug: ^slug) |> Repo.one!()

    articles =
      from(a in Article,
        join: at in "article_topics",
        on: at.article_id == a.id,
        where: at.topic_id == ^topic.id
      )
      |> newest()
      |> Repo.all()

    {topic, articles}
  end

  def search(q) do
    from(a in Article,
      where:
        fragment(
          "to_tsvector('english', ? || ' ' || coalesce(?, '') || ' ' || ?) @@ websearch_to_tsquery('english', ?)",
          a.title,
          a.summary,
          a.body_html,
          ^q
        )
    )
    |> newest()
    |> limit(20)
    |> Repo.all()
  end

  def update_title(slug, title) do
    Article |> where(slug: ^slug) |> Repo.one!() |> Article.title_changeset(%{title: title}) |> Repo.update()
  end

  @doc "Load the generated articles (the same file every system loads)."
  def seed(path) do
    data = path |> File.read!() |> Jason.decode!()
    now = DateTime.utc_now() |> DateTime.truncate(:second)

    {:ok, ada} =
      Repo.insert(%Author{slug: "ada", name: "Ada Smith"},
        on_conflict: {:replace, [:name]},
        conflict_target: :slug,
        returning: true
      )

    topics =
      data
      |> Enum.flat_map(& &1["topics"])
      |> Enum.uniq()
      |> Map.new(fn name ->
        slug = name |> String.downcase() |> String.replace(~r/[^a-z0-9]+/, "-")

        {:ok, t} =
          Repo.insert(%Topic{slug: slug, name: name},
            on_conflict: {:replace, [:name]},
            conflict_target: :slug,
            returning: true
          )

        {name, t}
      end)

    rows =
      Enum.map(data, fn a ->
        {:ok, dt, _} = DateTime.from_iso8601(date_time(a["date"]))

        %{
          slug: a["slug"],
          title: a["title"],
          summary: a["summary"],
          body_html: a["body_html"],
          author_id: ada.id,
          published_at: DateTime.truncate(dt, :second),
          inserted_at: now,
          updated_at: now
        }
      end)

    rows
    |> Enum.chunk_every(500)
    |> Enum.each(&Repo.insert_all(Article, &1, on_conflict: :nothing, conflict_target: :slug))

    ids = Article |> select([a], {a.slug, a.id}) |> Repo.all() |> Map.new()

    links =
      for a <- data, t <- a["topics"],
          do: %{article_id: ids[a["slug"]], topic_id: topics[t].id}

    links
    |> Enum.chunk_every(1000)
    |> Enum.each(&Repo.insert_all("article_topics", &1, on_conflict: :nothing))

    length(data)
  end

  defp date_time(d) when byte_size(d) == 10, do: d <> "T00:00:00Z"
  defp date_time(d), do: if(String.ends_with?(d, "Z") or String.contains?(d, "+"), do: d, else: d <> "Z")
end
