+++
title = "Search"
description = "Full-text search on PostgreSQL, results without JavaScript, the Listings components, and Pagefind for static sites."
weight = 17
+++

## On a site with a server

The `search` feature (in the `cms` and `app` presets) keeps a full-text index
of every *published* item: its title, its text (text and Markdown fields as
plain text, text inside blocks, term names) and its URL.

The index is a generated `tsvector` (titles weigh more than bodies) with a
GIN index; queries go through `websearch_to_tsquery`, so quotes, `or` and
`-word` work as people expect. `[search] language` picks the text search
configuration (`english` by default, `simple` for none). The index is an
*unlogged* table: it is derived from the content, so writing it costs no
write-ahead log and never waits for the disk. PostgreSQL empties unlogged
tables after a crash or failover (and standbys do not have them); `engine
serve` notices an empty index at start and within a minute and rebuilds it,
and a full reindex only writes the documents that changed.

```toml
[search]
language = "english"
path = "/search/"      # where search_box sends visitors
exclude = ["page"]     # types to leave out
```

`engine serve` updates the index from the same change feed that drives
publishing (edits in the admin, the API, the command line or SQL); `engine
build` and `engine search reindex` rebuild it whole. `engine search query
"iron gates"` tries a query from the command line.

A search page is a `request` route, so it works without JavaScript:

```toml
[[routes]]
path = "/search/"
template = "search.html"
mode = "request"
```

```jinja
{{ component("search_box", value=request.args.q) }}
{{ component("search_results", type="article", per_page=10) }}
```

`search_results` shows the hits with extracts around the matching words
(escaped, with `<mark>`), and page links. For your own markup,
`search(q, type=none, limit=10, page=1)` returns `query`, `total`, `pages`
and `hits` (`title`, `url`, `snippet`, `type`, `id`, `published_at`).

**What a search costs (PostgreSQL).** One statement per page of results:
PostgreSQL finds the matches (through the GIN index when the words are
selective, by scanning when most documents match), ranks every match with
`ts_rank`, counts them in the same pass (a window count, so the total needs
no second scan) and returns the page's rows with their text. The engine
then cuts the extracts itself: the query's words come back as PostgreSQL
normalised them, and a word of the text is marked when its Snowball stem
(the algorithm of PostgreSQL's built-in configurations) is one of them. Up
to two extracts of at most 24 words, joined by ` … `, as `ts_headline`
made them. This replaced `ts_headline`, which parses and stems each
document again (about 0.45 ms per 2.6 KB article on one core). It applies
to `simple` and the Snowball configurations (`english`, `french`, `german`,
`spanish`, `russian` and the other built-in ones); a custom configuration
(a thesaurus, `hunspell`, `unaccent`) keeps `ts_headline`, still only for
the page's rows. A test compares the engine's marks with `ts_headline`'s
on stemmed forms, hyphenated words, numbers, addresses and accented words;
they agree, except that the engine leaves words the query excludes
(`-railings`) unmarked where `ts_headline` marks them, and marks a phrase's
words (`"iron gate"`) wherever they occur. Each document's
`tsvector` is kept in its row rather than in TOAST storage, because ranking
reads it for every match.

## Listings components

| Component | Shows |
| --- | --- |
| `content_list` | items of a type as cards or a list (`type`, `limit`, `order`, `term`, `style`) |
| `filtered_list` | items with links to filter by a vocabulary's terms; on a request route the filter is `?term=`, on published pages the links go to the term pages |
| `tag_cloud` | a vocabulary's terms with their counts and a `data-weight` of 1 to 5 |
| `related_items` | items of the same type sharing the most terms with this one |
| `search_box` | a GET form to the search page |
| `search_results` | the results for `?q=` |
| `pagefind_search` | Pagefind's search box for static sites (below) |

They come with `content` (and `search` for the last three) and need no
JavaScript, except `pagefind_search`. The template functions behind them,
`term_counts(vocabulary)` and `related_items(type, id, limit=3)`, are there
for your own markup too.

## On a static site: Pagefind

A site without a server has no database to search. Set

```toml
[search]
pagefind = true
```

and `engine build` runs [Pagefind](https://pagefind.app/) over the built
site, which writes a static index to `public/pagefind/`, searched in the
browser. Put `{{ component("pagefind_search") }}` on a page. This needs
JavaScript in the visitor's browser; the component says so to those
without it.

Pagefind is a separate program and is not built into the engine (it would
add its whole indexer to every build for one optional step). `engine build`
looks for, in order: `WROUGHT_PAGEFIND` (a command line), `pagefind` on
`PATH`, or `npx` (it then runs the pinned `pagefind@1.5.2`). Without any of
them the build stops with a message saying how to install it.
