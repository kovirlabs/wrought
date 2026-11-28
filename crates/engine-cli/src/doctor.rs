//! `engine doctor`: the limits this engine sizes from the machine, each
//! with the formula and whether the site's configuration set it.

use std::path::Path;

use anyhow::Result;
use engine_core::limits::{self, Limits, LimitsConfig, SectionOverrides, Source};

pub fn run(site: &Path) -> Result<()> {
    let loaded = if site.join("site.toml").is_file() {
        let (cfg, _) = engine_core::builder::load_config(site)?;
        Some(cfg.site.name)
    } else {
        None
    };
    let (cfg, sections) = limits::overrides();
    let (cpus, memory) = engine_core::resources::describe();
    print!(
        "{}",
        report(
            &cpus,
            &memory,
            loaded.as_deref(),
            limits::current(),
            &cfg,
            &sections
        )
    );
    if loaded.is_some() {
        print!("{}", database(site));
    }
    Ok(())
}

/// Whether the site's database answers now, and what waits in the form
/// spool.
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
fn database(site: &Path) -> String {
    let mut out = String::from("\nDatabase\n");
    match crate::db::db_config(site) {
        Ok(None) => out.push_str("  none configured\n"),
        Err(e) => out.push_str(&format!("  configuration: {e:#}\n")),
        Ok(Some(cfg)) => {
            let started = std::time::Instant::now();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let r = rt.map_err(anyhow::Error::from).and_then(|rt| {
                rt.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        engine_db::Db::connect(&cfg),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("no answer within 5 s"))?
                })
            });
            match r {
                Ok(_) => out.push_str(&format!(
                    "  {}  reachable (connected in {} ms)\n",
                    cfg.redacted_url(),
                    started.elapsed().as_millis()
                )),
                Err(e) => out.push_str(&format!(
                    "  {}  UNREACHABLE: {e:#}\n  published pages are still served; pages \
                     rendered per request answer 503 and form posts wait in the spool\n",
                    cfg.redacted_url()
                )),
            }
        }
    }
    out.push_str(&sleep_report(site));
    let dir = std::env::var_os("ENGINE_FORM_SPOOL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| site.join(".engine/spool/forms"));
    let (n, bytes) = std::fs::read_dir(&dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .fold((0u64, 0u64), |(n, b), e| {
                    (n + 1, b + e.metadata().map(|m| m.len()).unwrap_or(0))
                })
        })
        .unwrap_or((0, 0));
    out.push_str(&format!(
        "  form spool  {n} post(s), {bytes} bytes waiting in {}\n",
        dir.display()
    ));
    let q = dir.join("quarantine");
    let set_aside = std::fs::read_dir(&q)
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    if set_aside > 0 {
        out.push_str(&format!(
            "  form spool  {set_aside} post(s) the database refused, set aside in {} (look at \
             them, then delete them)\n",
            q.display()
        ));
    }
    out
}

/// The sleeping database: the hosting admin's setting and the last state
/// the engine recorded (read from files: `doctor` never wakes it).
#[cfg(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
))]
fn sleep_report(site: &Path) -> String {
    let set = match engine_core::host::db_sleep() {
        Ok(None) => return "  sleep       off (the hosting admin's db_sleep_seconds)\n".into(),
        Ok(Some(s)) => s,
        Err(e) => return format!("  sleep       host settings: {e:#}\n"),
    };
    let mut out = format!(
        "  sleep       after {} s unused; wake: {}; sleep: {}\n",
        set.idle.as_secs(),
        set.wake.join(" "),
        set.sleep.join(" ")
    );
    let now = engine_db::sleep::unix_now();
    match engine_db::sleep::read_state(&site.join(".engine/db-sleep.json")) {
        Some(st) if st.asleep => {
            out.push_str(&format!("  sleep       asleep for {} s", now - st.since));
            if let Some(d) = st.next_due {
                out.push_str(&format!("; next job due in {} s", d - now));
            }
            out.push('\n');
            if let Some(e) = st.last_wake_error {
                out.push_str(&format!("  sleep       LAST WAKE FAILED: {e}\n"));
            }
        }
        _ => out.push_str("  sleep       awake (or never put to sleep)\n"),
    }
    out
}

#[cfg(not(any(
    feature = "content",
    feature = "auth",
    feature = "forms",
    feature = "jobs",
    feature = "media"
)))]
fn database(_site: &Path) -> String {
    String::new()
}

fn report(
    cpus: &str,
    memory: &str,
    site: Option<&str>,
    l: Limits,
    cfg: &LimitsConfig,
    sections: &SectionOverrides,
) -> String {
    let mut out = format!(
        "engine doctor (preset {})\n\nDetected\n  CPUs    {cpus}\n  memory  {memory}\n",
        engine_core::features::PRESET
    );
    out.push_str(&match site {
        Some(name) => format!("  site    {name} (its [limits] and other overrides applied)\n"),
        None => "  site    none here (no site.toml): the values without overrides\n".into(),
    });
    out.push_str("\nLimits (no separate small mode: the same formulas on every machine)\n");
    for e in l.explain(cfg, sections) {
        let why = match e.source {
            Source::Auto => format!("auto: {}", e.formula),
            Source::Configured(key) => format!("set by {key} (auto would be: {})", e.formula),
        };
        out.push_str(&format!("  {:<54} {:>12}   {why}\n", e.name, e.value));
    }
    out.push_str(&format!(
        "  {:<54} {:>12}   auto: eight per CPU (glibc's rule, on the CPUs this process may use)\n",
        "allocator arenas",
        8 * engine_core::resources::get().cpus
    ));
    out.push_str(
        "\nBackground work (image variants, compressing a bulk publish) runs at nice \
         10, so serving comes first.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::resources::Resources;

    #[test]
    fn report_says_what_and_why() {
        let r = Resources {
            cpus: 4,
            memory: 8 << 30,
        };
        let cfg = LimitsConfig {
            blocking_threads: Some(100),
            ..Default::default()
        };
        let sec = SectionOverrides {
            db_connections: Some(3),
            ..Default::default()
        };
        let l = Limits::size(r, &cfg, &sec);
        let text = report("4 (test)", "8192 MiB (test)", Some("Test"), l, &cfg, &sec);
        assert!(text.contains("CPUs    4 (test)"), "{text}");
        assert!(text.contains("set by [limits] blocking_threads"), "{text}");
        assert!(text.contains("set by [database] max_connections"), "{text}");
        assert!(text.contains("auto: one per CPU"), "{text}");
        assert!(text.contains("128 MiB"), "{text}");
        assert!(
            text.lines()
                .any(|l| l.contains("blocking threads") && l.contains("100"))
        );
    }
}
