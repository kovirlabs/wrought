//! `engine media`: add files to the library from the command line (seeding
//! a site, scripts) and list them. Uploads go through the same checks as
//! the admin's (content sniffing, limits, metadata stripping).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;
use engine_core::builder;
use engine_media::{MediaConfig, MediaStore, Upload};

use crate::SiteArg;

#[derive(Subcommand)]
pub enum MediaCmd {
    /// Add a file (JPEG, PNG, GIF, WebP or PDF) and print its number.
    Add {
        #[command(flatten)]
        site: SiteArg,
        /// The file to add.
        file: PathBuf,
        /// Description (alt text).
        #[arg(long, default_value = "")]
        alt: String,
    },
    /// List the library, newest first.
    List {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
}

/// The site's media store on its database.
pub fn store(site: &std::path::Path, db: &engine_db::Db) -> Result<MediaStore> {
    let (cfg, _) = builder::load_config(site)?;
    let mc = MediaConfig::from_site(cfg.media.as_ref())?;
    let dir = mc.dir_for(site);
    Ok(MediaStore::new(db.clone(), mc, dir))
}

pub fn media_cmd(cmd: MediaCmd) -> Result<()> {
    match cmd {
        MediaCmd::Add { site, file, alt } => {
            let c = crate::db::connect(&site.site)?;
            c.ensure_migrated()?;
            let store = store(&site.site, &c.db)?;
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let m =
                c.rt.block_on(store.upload(Upload {
                    bytes: &bytes,
                    filename: &name,
                    alt: &alt,
                    private: false,
                    uploaded_by: None,
                }))
                .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
            println!(
                "Added #{} {} ({} copies){}",
                m.id,
                m.url().unwrap_or_default(),
                m.variants.len(),
                if m.width.is_some() {
                    format!(", {}×{}", m.width.unwrap_or(0), m.height.unwrap_or(0))
                } else {
                    String::new()
                }
            );
        }
        MediaCmd::List { site, limit } => {
            let c = crate::db::connect(&site.site)?;
            c.ensure_migrated()?;
            let store = store(&site.site, &c.db)?;
            for m in c.rt.block_on(store.list(false, None, limit, 0))? {
                println!(
                    "#{:<5} {:<40} {:<16} {}",
                    m.id,
                    m.url().unwrap_or_default(),
                    m.mime,
                    m.alt
                );
            }
        }
    }
    Ok(())
}
