class ArticlesController < ApplicationController
  PER_PAGE = 10

  def home
    @articles = Article.newest.includes(:author).limit(3)
  end

  def index
    @page = [params.fetch(:page, 1).to_i, 1].max
    @total_pages = (Article.count / PER_PAGE.to_f).ceil
    @articles = Article.newest.includes(:author).offset((@page - 1) * PER_PAGE).limit(PER_PAGE)
    http_cache(@articles) unless performed?
  end

  def show
    @article = Article.includes(:author, :topics).find_by!(slug: params[:slug])
    http_cache(@article)
  end
end
