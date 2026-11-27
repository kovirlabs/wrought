//! The benchmark pages, server-rendered with Loco's Tera views: home,
//! article listing (10 per page), article, topic, search (PostgreSQL full
//! text, as the Rails and Phoenix apps), and the token-protected title edit
//! of the "edit to published" test.
use axum::http::HeaderMap;
use loco_rs::prelude::*;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};

type Ts = sea_orm::prelude::DateTimeWithTimeZone;

#[derive(FromQueryResult)]
struct CardRow {
    id: i64,
    slug: String,
    title: String,
    summary: Option<String>,
    published_at: Ts,
    author: Option<String>,
}

#[derive(Serialize)]
struct Card {
    slug: String,
    title: String,
    summary: String,
    iso: String,
    date: String,
    author: Option<String>,
}

impl From<CardRow> for Card {
    fn from(r: CardRow) -> Self {
        Card {
            slug: r.slug,
            title: r.title,
            summary: r.summary.unwrap_or_default(),
            iso: r.published_at.format("%Y-%m-%d").to_string(),
            date: r.published_at.format("%-d %B %Y").to_string(),
            author: r.author,
        }
    }
}

#[derive(FromQueryResult, Serialize)]
struct Link {
    slug: String,
    name: String,
}

#[derive(FromQueryResult)]
struct Body {
    body_html: String,
}

const CARD: &str = "SELECT a.id, a.slug, a.title, a.summary, a.published_at, au.name AS author \
                    FROM articles a LEFT JOIN authors au ON au.id = a.author_id";
const NEWEST: &str = "ORDER BY a.published_at DESC, a.id DESC";
const PER_PAGE: i64 = 10;

fn pg(sql: &str, v: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, v)
}

async fn cards(ctx: &AppContext, sql: String, v: Vec<sea_orm::Value>) -> Result<Vec<CardRow>> {
    Ok(CardRow::find_by_statement(pg(&sql, v)).all(&ctx.db).await?)
}

#[debug_handler]
async fn home(ViewEngine(v): ViewEngine<TeraView>, State(ctx): State<AppContext>) -> Result<Response> {
    let a: Vec<Card> = cards(&ctx, format!("{CARD} {NEWEST} LIMIT 3"), vec![])
        .await?
        .into_iter()
        .map(Card::from)
        .collect();
    format::render().view(&v, "bench/home.html", data!({ "articles": a }))
}

async fn listing(v: TeraView, ctx: AppContext, page: i64) -> Result<Response> {
    let page = page.max(1);
    let total: i64 = ctx
        .db
        .query_one_raw(pg("SELECT count(*) AS n FROM articles", vec![]))
        .await?
        .map(|r| r.try_get("", "n"))
        .transpose()?
        .unwrap_or(0);
    let pages = (total + PER_PAGE - 1) / PER_PAGE;
    let a: Vec<Card> = cards(
        &ctx,
        format!("{CARD} {NEWEST} LIMIT $1 OFFSET $2"),
        vec![PER_PAGE.into(), ((page - 1) * PER_PAGE).into()],
    )
    .await?
    .into_iter()
    .map(Card::from)
    .collect();
    format::render().view(
        &v,
        "bench/index.html",
        data!({ "articles": a, "page": page, "pages": pages }),
    )
}

#[debug_handler]
async fn index(ViewEngine(v): ViewEngine<TeraView>, State(ctx): State<AppContext>) -> Result<Response> {
    listing(v, ctx, 1).await
}

#[debug_handler]
async fn index_page(
    ViewEngine(v): ViewEngine<TeraView>,
    State(ctx): State<AppContext>,
    Path(page): Path<i64>,
) -> Result<Response> {
    listing(v, ctx, page).await
}

#[debug_handler]
async fn show(
    ViewEngine(v): ViewEngine<TeraView>,
    State(ctx): State<AppContext>,
    Path(slug): Path<String>,
) -> Result<Response> {
    let Some(row) = cards(&ctx, format!("{CARD} WHERE a.slug = $1"), vec![slug.into()])
        .await?
        .into_iter()
        .next()
    else {
        return not_found();
    };
    let id = row.id;
    let body = Body::find_by_statement(pg("SELECT body_html FROM articles WHERE id = $1", vec![id.into()]))
        .one(&ctx.db)
        .await?
        .map(|b| b.body_html)
        .unwrap_or_default();
    let topics = Link::find_by_statement(pg(
        "SELECT t.slug, t.name FROM topics t JOIN article_topics at ON at.topic_id = t.id \
         WHERE at.article_id = $1 ORDER BY t.id",
        vec![id.into()],
    ))
    .all(&ctx.db)
    .await?;
    // Up to three articles sharing the most topics (the same rule as
    // Wrought's related_items and the Rails app).
    let related = Link::find_by_statement(pg(
        "SELECT a.slug, a.title AS name FROM (SELECT at.article_id, count(*) AS n FROM article_topics at \
         WHERE at.topic_id IN (SELECT topic_id FROM article_topics WHERE article_id = $1) \
         AND at.article_id <> $1 GROUP BY at.article_id ORDER BY count(*) DESC, at.article_id DESC LIMIT 3) r \
         JOIN articles a ON a.id = r.article_id ORDER BY r.n DESC, r.article_id DESC",
        vec![id.into()],
    ))
    .all(&ctx.db)
    .await?;
    format::render().view(
        &v,
        "bench/show.html",
        data!({ "article": Card::from(row), "body": body, "topics": topics, "related": related }),
    )
}

#[debug_handler]
async fn topic(
    ViewEngine(v): ViewEngine<TeraView>,
    State(ctx): State<AppContext>,
    Path(slug): Path<String>,
) -> Result<Response> {
    let Some(t) = Link::find_by_statement(pg("SELECT slug, name FROM topics WHERE slug = $1", vec![slug.into()]))
        .one(&ctx.db)
        .await?
    else {
        return not_found();
    };
    let a: Vec<Card> = cards(
        &ctx,
        format!(
            "{CARD} JOIN article_topics at ON at.article_id = a.id JOIN topics t ON t.id = at.topic_id \
             WHERE t.slug = $1 {NEWEST}"
        ),
        vec![t.slug.clone().into()],
    )
    .await?
    .into_iter()
    .map(Card::from)
    .collect();
    format::render().view(&v, "bench/topic.html", data!({ "topic": t, "articles": a }))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

#[debug_handler]
async fn search(
    ViewEngine(v): ViewEngine<TeraView>,
    State(ctx): State<AppContext>,
    Query(p): Query<SearchQuery>,
) -> Result<Response> {
    let q: String = p.q.unwrap_or_default().trim().chars().take(200).collect();
    let results: Vec<Card> = if q.is_empty() {
        Vec::new()
    } else {
        cards(
            &ctx,
            format!(
                "{CARD} WHERE to_tsvector('english', a.title || ' ' || coalesce(a.summary, '') || ' ' || a.body_html) \
                 @@ websearch_to_tsquery('english', $1) {NEWEST} LIMIT 20"
            ),
            vec![q.clone().into()],
        )
        .await?
        .into_iter()
        .map(Card::from)
        .collect()
    };
    format::render().view(&v, "bench/search.html", data!({ "q": q, "results": results }))
}

#[derive(Deserialize)]
struct Edit {
    title: String,
}

#[debug_handler]
async fn update(
    State(ctx): State<AppContext>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Json(e): Json<Edit>,
) -> Result<Response> {
    let expected = std::env::var("BENCH_API_TOKEN").unwrap_or_default();
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default();
    if expected.is_empty() || token != expected {
        return unauthorized("bad token");
    }
    ctx.db
        .execute_raw(pg(
            "UPDATE articles SET title = $1, updated_at = now() WHERE slug = $2",
            vec![e.title.clone().into(), slug.clone().into()],
        ))
        .await?;
    format::json(serde_json::json!({ "slug": slug, "title": e.title }))
}

// Loco registers routes without a trailing slash (`/articles/`, as the
// other apps write it, is `/articles` here); the bench requests each app's
// own form of the same paths.
pub fn routes() -> Routes {
    Routes::new()
        .add("/", get(home))
        .add("/articles/", get(index))
        .add("/articles/page/{page}/", get(index_page))
        .add("/articles/{slug}/", get(show))
        .add("/topics/{slug}/", get(topic))
        .add("/search/", get(search))
        .add("/api/articles/{slug}", patch(update))
}
