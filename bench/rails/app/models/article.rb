class Article < ApplicationRecord
  belongs_to :author, optional: true, touch: true
  has_many :article_topics, dependent: :delete_all
  has_many :topics, through: :article_topics

  scope :newest, -> { order(published_at: :desc, id: :desc) }

  # Up to three articles sharing the most topics with this one (the same
  # rule as Wrought's related_items).
  def related(limit = 3)
    ids = ArticleTopic.where(topic_id: topic_ids).where.not(article_id: id)
      .group(:article_id).order(Arel.sql("count(*) DESC, article_id DESC"))
      .limit(limit).pluck(:article_id)
    Article.where(id: ids).index_by(&:id).values_at(*ids).compact
  end

  def self.search(q)
    where("to_tsvector('english', title || ' ' || coalesce(summary, '') || ' ' || body_html) " \
          "@@ websearch_to_tsquery('english', ?)", q)
  end

  def to_param = slug
end
