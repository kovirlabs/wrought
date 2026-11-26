class CreateBench < ActiveRecord::Migration[8.1]
  def change
    create_table :authors do |t|
      t.string :slug, null: false, index: { unique: true }
      t.string :name, null: false
      t.timestamps
    end
    create_table :articles do |t|
      t.string :slug, null: false, index: { unique: true }
      t.string :title, null: false
      t.text :summary
      t.text :body_html, null: false
      t.references :author, foreign_key: true
      t.datetime :published_at, null: false, index: true
      t.timestamps
    end
    create_table :topics do |t|
      t.string :slug, null: false, index: { unique: true }
      t.string :name, null: false
      t.timestamps
    end
    create_table :article_topics, id: false do |t|
      t.references :article, null: false, foreign_key: true
      t.references :topic, null: false, foreign_key: true, index: true
    end
    add_index :article_topics, [:article_id, :topic_id], unique: true
    # Full-text search, as PostgreSQL's documentation describes it: a GIN
    # index on the same expression the query uses.
    execute <<~SQL
      CREATE INDEX articles_search ON articles USING gin
        (to_tsvector('english', title || ' ' || coalesce(summary, '') || ' ' || body_html));
    SQL
  end
end
