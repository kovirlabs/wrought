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
    Ok(())
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
