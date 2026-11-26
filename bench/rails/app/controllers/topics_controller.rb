class TopicsController < ApplicationController
  def show
    @topic = Topic.find_by!(slug: params[:slug])
    @articles = @topic.articles.newest
  end
end
