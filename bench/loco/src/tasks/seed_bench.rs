//! `bench-cli task seed_bench path:/bench/data/articles-200.json`: the
//! generated articles (the same file every system loads), one author, their
//! topics.
use loco_rs::prelude::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

pub struct SeedBench;

#[async_trait]
impl Task for SeedBench {
    fn task(&self) -> TaskInfo {
        TaskInfo {
            name: "seed_bench".to_string(),
            detail: "Load the benchmark articles: seed_bench path:<file.json>".to_string(),
        }
    }

    async fn run(&self, ctx: &AppContext, vars: &task::Vars) -> Result<()> {
        let path = vars.cli_arg("path")?;
        let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let arts = data.as_array().ok_or_else(|| Error::string("not a list"))?;
        let db = &ctx.db;
        let pg = |sql: &str, v: Vec<sea_orm::Value>| Statement::from_sql_and_values(DbBackend::Postgres, sql, v);
        db.execute_raw(pg(
            "INSERT INTO authors (slug, name) VALUES ('ada', 'Ada Smith') ON CONFLICT (slug) DO NOTHING",
            vec![],
        ))
        .await?;
        let s = |a: &serde_json::Value, k: &str| a[k].as_str().unwrap_or_default().to_string();
        let mut names: Vec<String> = arts
            .iter()
            .flat_map(|a| a["topics"].as_array().cloned().unwrap_or_default())
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        names.sort();
        names.dedup();
        let slugs: Vec<String> = names
            .iter()
            .map(|n| {
                n.to_lowercase()
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join("-")
            })
            .collect();
        db.execute_raw(pg(
            "INSERT INTO topics (slug, name) SELECT * FROM unnest($1::text[], $2::text[]) ON CONFLICT (slug) DO NOTHING",
            vec![slugs.into(), names.clone().into()],
        ))
        .await?;
        let col = |k: &str| arts.iter().map(|a| s(a, k)).collect::<Vec<_>>();
        db.execute_raw(pg(
            "INSERT INTO articles (slug, title, summary, body_html, author_id, published_at)
             SELECT a.slug, a.title, a.summary, a.body, (SELECT id FROM authors WHERE slug = 'ada'), a.d::date
             FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[]) AS a(slug, title, summary, body, d)
             ON CONFLICT (slug) DO NOTHING",
            vec![col("slug").into(), col("title").into(), col("summary").into(), col("body_html").into(), col("date").into()],
        ))
        .await?;
        let (mut art, mut top) = (Vec::new(), Vec::new());
        for a in arts {
            for t in a["topics"].as_array().into_iter().flatten() {
                art.push(s(a, "slug"));
                top.push(t.as_str().unwrap_or_default().to_string());
            }
        }
        db.execute_raw(pg(
            "INSERT INTO article_topics (article_id, topic_id)
             SELECT a.id, t.id FROM unnest($1::text[], $2::text[]) AS l(slug, topic)
             JOIN articles a ON a.slug = l.slug JOIN topics t ON t.name = l.topic
             ON CONFLICT DO NOTHING",
            vec![art.into(), top.into()],
        ))
        .await?;
        println!("seeded {} articles", arts.len());
        Ok(())
    }
}
