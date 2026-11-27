defmodule Bench.Repo.Migrations.CreateBench do
  use Ecto.Migration

  def change do
    create table(:authors) do
      add :slug, :string, null: false
      add :name, :string, null: false
      timestamps(type: :utc_datetime)
    end

    create unique_index(:authors, [:slug])

    create table(:articles) do
      add :slug, :string, null: false
      add :title, :string, null: false
      add :summary, :text
      add :body_html, :text, null: false
      add :author_id, references(:authors)
      add :published_at, :utc_datetime, null: false
      timestamps(type: :utc_datetime)
    end

    create unique_index(:articles, [:slug])
    create index(:articles, [:published_at])
    create index(:articles, [:author_id])

    create table(:topics) do
      add :slug, :string, null: false
      add :name, :string, null: false
      timestamps(type: :utc_datetime)
    end

    create unique_index(:topics, [:slug])

    create table(:article_topics, primary_key: false) do
      add :article_id, references(:articles), null: false
      add :topic_id, references(:topics), null: false
    end

    create unique_index(:article_topics, [:article_id, :topic_id])
    create index(:article_topics, [:topic_id])

    # Full-text search as PostgreSQL's documentation describes it (the same
    # index as the Rails app): a GIN index on the expression the query uses.
    execute(
      "CREATE INDEX articles_search ON articles USING gin " <>
        "(to_tsvector('english', title || ' ' || coalesce(summary, '') || ' ' || body_html))",
      "DROP INDEX articles_search"
    )
  end
end
