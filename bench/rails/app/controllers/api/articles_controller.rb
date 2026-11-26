# The edit used by the "edit to published" test: a token-protected JSON
# update, as a Rails app would expose to its own tools.
class Api::ArticlesController < ActionController::API
  before_action do
    token = request.authorization.to_s.delete_prefix("Bearer ")
    expected = ENV.fetch("BENCH_API_TOKEN")
    head :unauthorized unless ActiveSupport::SecurityUtils.secure_compare(token, expected)
  end

  def update
    article = Article.find_by!(slug: params[:slug])
    article.update!(title: params.require(:title))
    render json: { slug: article.slug, title: article.title }
  end
end
