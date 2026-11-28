//! Custom App: a Hammerwright site as its own Rust crate.
//!
//! The binary is the whole `engine` command line (`new`, `dev`, `build`,
//! `serve`, `migrate`, `doctor`...) plus what this site registers:
//!
//! * a handler: `POST /signups/` takes `{"email": "..."}`, stores it and
//!   queues the welcome job; `GET /signups/{id}/` shows it, rendered with
//!   the site's templates;
//! * named queries: `recent_signups`, which the home page lists (tagged
//!   `c:signups`, so a signup re-publishes the page), `signup_count` and
//!   `signup` (used by the handler too);
//! * a job, `signup.welcome`, on the engine's PostgreSQL queue;
//! * a hook after each publish batch, logging the pages it wrote;
//! * a command, `signups`, printing the latest signups;
//! * the site's own tables, as a migration embedded in the binary.

use std::process::ExitCode;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use engine_cli::app::{Context, HandlerResult, JobContext, TaskContext};
use engine_cli::{App, anyhow, clap, engine_db, minijinja, serde_json};

fn main() -> ExitCode {
    app().run()
}

/// Everything this site adds to the engine.
fn app() -> App {
    App::new()
        .migration("0001_signups", include_str!("migrations/0001_signups.sql"))
        .route("/signups/", post(add_signup))
        .route("/signups/{id}/", get(show_signup))
        .query(
            "recent_signups",
            "SELECT id, email, welcomed_at IS NOT NULL AS welcomed
             FROM app_signups ORDER BY id DESC LIMIT $1",
            &["c:signups"],
        )
        .query(
            "signup",
            "SELECT id, email, welcomed_at IS NOT NULL AS welcomed FROM app_signups WHERE id = $1",
            &["c:signups"],
        )
        .query(
            "signup_count",
            "SELECT count(*) AS n FROM app_signups",
            &["c:signups"],
        )
        .job("signup.welcome", welcome)
        .on_publish(|cx: Context, report| async move {
            for path in &report.rendered {
                engine_db::query("INSERT INTO app_publish_log (path, reason) VALUES ($1, $2)")
                    .bind(path.as_str())
                    .bind(report.reason.as_str())
                    .execute(cx.db())
                    .await?;
            }
            Ok(())
        })
        .command(
            clap::Command::new("signups")
                .about("List the latest signups")
                .arg(
                    clap::Arg::new("limit")
                        .long("limit")
                        .default_value("10")
                        .value_parser(clap::value_parser!(i64)),
                ),
            list_signups,
        )
}

#[derive(serde::Deserialize)]
struct NewSignup {
    email: String,
}

/// `POST /signups/` with `{"email": "..."}`: `201` and the new id, `409`
/// if the address signed up before.
async fn add_signup(
    State(cx): State<Context>,
    headers: HeaderMap,
    Json(body): Json<NewSignup>,
) -> HandlerResult<impl IntoResponse> {
    // A browser's cross-site post is refused (a JSON API without cookies
    // would not need it; it costs nothing).
    if !engine_cli::engine_core::web::same_origin(&headers) {
        return Ok((StatusCode::FORBIDDEN, Json(serde_json::json!({}))));
    }
    let email = body.email.trim().to_lowercase();
    if email.len() > 254 || !email.contains('@') {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "an email address, please" })),
        ));
    }
    let id: Option<i64> = engine_db::query(
        "INSERT INTO app_signups (email) VALUES ($1)
         ON CONFLICT (email) DO NOTHING RETURNING id",
    )
    .bind(email.as_str())
    .scalar_opt(cx.db())
    .await?;
    let Some(id) = id else {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "already signed up" })),
        ));
    };
    cx.enqueue("signup.welcome", serde_json::json!({ "id": id }))
        .await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

/// `GET /signups/{id}/`: the signup, in the site's layout.
async fn show_signup(
    State(cx): State<Context>,
    Path(id): Path<i64>,
) -> HandlerResult<axum::response::Response> {
    let rows = cx.query("signup", vec![id.into()]).await?;
    let Some(signup) = rows.into_iter().next() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let page = cx
        .render(
            "signup.html",
            minijinja::context! { signup => minijinja::Value::from_serialize(&signup) },
        )
        .await?;
    Ok(page.into_response())
}

/// The `signup.welcome` job: mark the signup welcomed (a real site would
/// send the welcome email here, through `cx.mailer`).
async fn welcome(cx: JobContext) -> anyhow::Result<()> {
    let id = cx.job.payload["id"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("payload without an id"))?;
    engine_db::query("UPDATE app_signups SET welcomed_at = now() WHERE id = $1")
        .bind(id)
        .execute(&cx.db)
        .await?;
    Ok(())
}

/// `custom-app signups [--limit N]`.
fn list_signups(cx: &TaskContext) -> anyhow::Result<()> {
    let limit = *cx.args.get_one::<i64>("limit").expect("has a default");
    let c = cx.connect()?;
    let (total, emails) = c.rt.block_on(async {
        let total: i64 = engine_db::query("SELECT count(*) FROM app_signups")
            .scalar(&c.db)
            .await?;
        let emails: Vec<String> =
            engine_db::query("SELECT email FROM app_signups ORDER BY id DESC LIMIT $1")
                .bind(limit)
                .scalars(&c.db)
                .await?;
        anyhow::Ok((total, emails))
    })?;
    println!("{total} signup(s)");
    for e in emails {
        println!("  {e}");
    }
    Ok(())
}
