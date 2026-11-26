class SearchController < ApplicationController
  def show
    @q = params[:q].to_s.strip[0, 200]
    @results = @q.empty? ? Article.none : Article.search(@q).newest.limit(20)
  end
end
