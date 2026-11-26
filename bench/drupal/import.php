<?php
// `drush php:script import.php /bench/data/articles.json`: one published
// article node per article, with a URL alias, its topics as tags and the
// body as HTML (converted by run.py).
use Drupal\node\Entity\Node;
use Drupal\taxonomy\Entity\Term;
$articles = json_decode(file_get_contents($extra[0]), true);
$terms = [];
foreach ($articles as $a) {
  $tags = [];
  foreach ($a['topics'] as $t) {
    if (!isset($terms[$t])) {
      $term = Term::create(['vid' => 'tags', 'name' => $t]);
      $term->save();
      $terms[$t] = $term->id();
    }
    $tags[] = ['target_id' => $terms[$t]];
  }
  Node::create([
    'type' => 'article',
    'title' => $a['title'],
    'body' => ['value' => $a['body_html'], 'summary' => $a['summary'], 'format' => 'full_html'],
    'field_tags' => $tags,
    'status' => 1,
    'created' => strtotime($a['date'] . ' 12:00:00'),
    'path' => ['alias' => '/' . $a['slug']],
  ])->save();
}
echo count($articles) . " nodes\n";
