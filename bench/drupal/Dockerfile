# Drupal (official image, pinned) plus Drush for the unattended install and
# the import. Nothing else is changed from the defaults.
FROM drupal:11.3.23-php8.4-apache-bookworm
WORKDIR /opt/drupal
RUN composer require --no-interaction --no-progress drush/drush:^13 \
 && ln -s /opt/drupal/vendor/bin/drush /usr/local/bin/drush
