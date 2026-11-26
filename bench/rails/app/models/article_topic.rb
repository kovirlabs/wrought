class ArticleTopic < ApplicationRecord
  belongs_to :article
  belongs_to :topic, touch: true
end
