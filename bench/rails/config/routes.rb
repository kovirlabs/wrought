Rails.application.routes.draw do
  root "articles#home"
  get "articles/" => "articles#index", as: :articles
  get "articles/page/:page/" => "articles#index", as: :articles_page
  get "articles/:slug/" => "articles#show", as: :article
  get "topics/:slug/" => "topics#show", as: :topic
  get "search/" => "search#show", as: :search
  namespace :api do
    patch "articles/:slug" => "articles#update"
  end
  get "up" => "rails/health#show", as: :rails_health_check
end
