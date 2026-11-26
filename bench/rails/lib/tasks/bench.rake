# bin/rails "bench:seed[/bench/data/articles-200.json]": the generated
# articles (the same file every system loads), one author, their topics.
namespace :bench do
  task :seed, [:file] => :environment do |_, args|
    data = JSON.parse(File.read(args.fetch(:file)))
    now = Time.current
    ada = Author.find_or_create_by!(slug: "ada") { |a| a.name = "Ada Smith" }
    topics = {}
    data.flat_map { |a| a["topics"] }.uniq.each do |name|
      topics[name] = Topic.find_or_create_by!(slug: name.downcase.gsub(/[^a-z0-9]+/, "-")) { |t| t.name = name }
    end
    rows = data.map do |a|
      { slug: a["slug"], title: a["title"], summary: a["summary"], body_html: a["body_html"],
        author_id: ada.id, published_at: Time.zone.parse(a["date"]), created_at: now, updated_at: now }
    end
    Article.insert_all(rows, unique_by: :slug)
    ids = Article.where(slug: data.map { |a| a["slug"] }).pluck(:slug, :id).to_h
    links = data.flat_map { |a| a["topics"].map { |t| { article_id: ids[a["slug"]], topic_id: topics[t].id } } }
    ArticleTopic.insert_all(links)
    puts "seeded #{data.size} articles"
  end
end
