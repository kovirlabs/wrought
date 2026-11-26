<?php
// `wp eval-file import.php /bench/data/articles.json`: one published post
// per article, its topics as tags, the body as HTML (converted by run.py).
$articles = json_decode(file_get_contents($args[0]), true);
wp_defer_term_counting(true);
foreach ($articles as $a) {
    $id = wp_insert_post([
        'post_title' => $a['title'],
        'post_name' => $a['slug'],
        'post_excerpt' => $a['summary'],
        'post_content' => $a['body_html'],
        'post_status' => 'publish',
        'post_date' => $a['date'] . ' 12:00:00',
        'tags_input' => $a['topics'],
    ], true);
    if (is_wp_error($id)) { fwrite(STDERR, $id->get_error_message() . "\n"); exit(1); }
}
wp_defer_term_counting(false);
echo count($articles) . " posts\n";
