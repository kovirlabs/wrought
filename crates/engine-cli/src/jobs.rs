//! `engine jobs` and the job handlers `engine serve` runs (feature `jobs`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Subcommand;
use engine_core::builder;
use engine_core::config::SiteConfig;
use engine_db::Db;
use engine_jobs::{Handlers, Jobs, JobsConfig, Schedule};

use crate::SiteArg;

#[derive(Subcommand)]
pub enum JobsCmd {
    /// List jobs, newest first.
    List {
        #[command(flatten)]
        site: SiteArg,
        /// queued, running, done or dead.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Queue a dead job again with fresh attempts.
    Retry {
        #[command(flatten)]
        site: SiteArg,
        id: i64,
    },
    /// Run job workers only (no web server), e.g. as a second process.
    Work {
        #[command(flatten)]
        site: SiteArg,
    },
}

/// The queue, its handlers and (with `mail`) the mailer of a site.
pub struct Parts {
    pub handlers: Handlers,
    pub config: JobsConfig,
    #[cfg(feature = "mail")]
    pub mailer: engine_mail::Mailer,
}

/// The engine's own recurring jobs.
pub fn engine_schedules() -> Vec<Schedule> {
    #[allow(unused_mut)]
    let mut v = vec![Schedule {
        name: "engine.cleanup".into(),
        kind: "engine.cleanup".into(),
        every: Duration::from_secs(3600),
        payload: serde_json::json!({}),
    }];
    v
}

/// Handlers of every compiled feature for a site.
#[allow(unused_variables)]
pub fn parts(site: &Path, cfg: &SiteConfig, db: &Db, public_url: &str) -> Result<Parts> {
    let config = JobsConfig::from_site(cfg.jobs.as_ref())?;
    let jobs = Jobs::new(db.clone());
    let mut handlers = Handlers::new();
    let j = jobs.clone();
    #[cfg(feature = "forms")]
    let forms = engine_forms::FormStore::new(db.clone());
    handlers.register("engine.cleanup", move |_ctx| {
        let j = j.clone();
        #[cfg(feature = "forms")]
        let forms = forms.clone();
        async move {
            // Finished jobs a week, dead ones a month.
            let n = j
                .prune(
                    Duration::from_secs(7 * 86_400),
                    Duration::from_secs(30 * 86_400),
                )
                .await?;
            #[cfg(feature = "forms")]
            forms.prune_events().await?;
            if n > 0 {
                eprintln!("jobs: removed {n} finished job(s)");
            }
            Ok(())
        }
    });
    // Applies payment provider events to orders (`payments` feature).
    #[cfg(feature = "payments")]
    engine_payments::register_jobs(&mut handlers, db.clone());
    #[cfg(feature = "mail")]
    let mailer = {
        let mc = engine_mail::MailConfig::from_site(cfg.mail.as_ref())?;
        let m = engine_mail::Mailer::new(jobs.clone(), mc, site, &cfg.site.name, public_url)?;
        m.register(&mut handlers);
        m
    };
    Ok(Parts {
        handlers,
        config,
        #[cfg(feature = "mail")]
        mailer,
    })
}

pub fn jobs_cmd(cmd: JobsCmd) -> Result<()> {
    match cmd {
        JobsCmd::List {
            site,
            status,
            limit,
        } => {
            if let Some(s) = &status
                && !["queued", "running", "done", "dead"].contains(&s.as_str())
            {
                bail!("--status is queued, running, done or dead");
            }
            let c = crate::db::connect(&site.site)?;
            c.ensure_migrated()?;
            let jobs = Jobs::new(c.db.clone());
            let list = c.rt.block_on(jobs.list(status.as_deref(), limit))?;
            for (s, n) in c.rt.block_on(jobs.counts())? {
                print!("{s}: {n}  ");
            }
            println!();
            for j in list {
                println!(
                    "{:>6}  {:<8} {:<20} attempts {}/{}  run_at {}{}",
                    j.id,
                    j.status,
                    j.kind,
                    j.attempts,
                    j.max_attempts,
                    j.run_at,
                    j.last_error
                        .map(|e| format!("\n        last error: {e}"))
                        .unwrap_or_default()
                );
            }
        }
        JobsCmd::Retry { site, id } => {
            let c = crate::db::connect(&site.site)?;
            c.ensure_migrated()?;
            if !c.rt.block_on(Jobs::new(c.db.clone()).retry(id))? {
                bail!("job {id} is not dead (only dead jobs can be retried)");
            }
            println!("Job {id} queued again.");
        }
        JobsCmd::Work { site } => {
            let c = crate::db::connect(&site.site)?;
            c.ensure_migrated()?;
            let (cfg, _) = builder::load_config(&site.site)?;
            let public_url = std::env::var("ENGINE_PUBLIC_URL")
                .ok()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| cfg.site.base_url.clone());
            let p = parts(&site.site, &cfg, &c.db, &public_url)?;
            let mut wo = engine_jobs::WorkerOptions::from_config(&p.config)?;
            wo.schedules.extend(engine_schedules());
            println!(
                "Job workers: {} at a time, kinds: {} (Ctrl+C to stop)",
                wo.concurrency,
                p.handlers.kinds().join(", ")
            );
            c.rt.block_on(engine_jobs::run(
                c.db.clone(),
                p.handlers,
                wo,
                crate::shutdown_signal(),
            ))?;
        }
    }
    Ok(())
}
