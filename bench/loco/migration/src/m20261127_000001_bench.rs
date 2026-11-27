use sea_orm_migration::prelude::*;

/// The benchmark's tables: the same schema as the Rails and Phoenix apps,
/// with the full-text GIN index PostgreSQL's documentation describes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(
                r"
CREATE TABLE authors (id bigserial PRIMARY KEY, slug varchar NOT NULL UNIQUE,
  name varchar NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE articles (id bigserial PRIMARY KEY, slug varchar NOT NULL UNIQUE,
  title varchar NOT NULL, summary text, body_html text NOT NULL,
  author_id bigint REFERENCES authors, published_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE INDEX articles_published_at ON articles (published_at);
CREATE INDEX articles_author_id ON articles (author_id);
CREATE TABLE topics (id bigserial PRIMARY KEY, slug varchar NOT NULL UNIQUE,
  name varchar NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE article_topics (article_id bigint NOT NULL REFERENCES articles,
  topic_id bigint NOT NULL REFERENCES topics, UNIQUE (article_id, topic_id));
CREATE INDEX article_topics_topic_id ON article_topics (topic_id);
CREATE INDEX articles_search ON articles USING gin
  (to_tsvector('english', title || ' ' || coalesce(summary, '') || ' ' || body_html));
",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared("DROP TABLE article_topics, topics, articles, authors")
            .await?;
        Ok(())
    }
}
