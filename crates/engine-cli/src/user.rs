//! `engine user`: accounts from the command line, including the first
//! administrator (there are never default credentials).

use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use engine_auth::{Auth, AuthConfig, AuthError, LogNotifier, UserChanges};
use engine_core::builder;

use crate::SiteArg;
use crate::db::connect;

#[derive(Subcommand)]
pub enum UserCmd {
    /// Create an account. Without --password-stdin it gets no usable
    /// password and a single-use link to choose one is printed. The first
    /// account becomes an administrator.
    Create {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long)]
        email: String,
        #[arg(long, default_value = "")]
        name: String,
        /// Role to give (repeatable; `roles` feature). Default for the first
        /// account: admin.
        #[arg(long = "role", value_name = "ROLE")]
        roles: Vec<String>,
        /// Read the password from the first line of standard input.
        #[arg(long)]
        password_stdin: bool,
    },
    /// List accounts.
    List {
        #[command(flatten)]
        site: SiteArg,
    },
    /// Print a single-use link (valid one hour) to set an account's password.
    ResetLink {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long)]
        email: String,
    },
    /// Change an account's roles or turn it on or off.
    Set {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long)]
        email: String,
        /// Replace the account's roles (repeatable; `roles` feature).
        #[arg(long = "role", value_name = "ROLE")]
        roles: Vec<String>,
        /// Remove every role.
        #[arg(long, conflicts_with = "roles")]
        no_roles: bool,
        /// Allow (true) or block (false) signing in.
        #[arg(long)]
        active: Option<bool>,
    },
    /// Turn off two-factor sign-in (for a lost authenticator).
    TotpOff {
        #[command(flatten)]
        site: SiteArg,
        #[arg(long)]
        email: String,
    },
}

pub(crate) fn auth_for(site: &Path, db: engine_db::Db) -> Result<Auth> {
    let (cfg, _) = builder::load_config(site)?;
    let url = std::env::var("ENGINE_PUBLIC_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            if cfg.site.base_url.is_empty() {
                "http://127.0.0.1:4400".into()
            } else {
                cfg.site.base_url.clone()
            }
        });
    Ok(Auth::new(
        db,
        AuthConfig::from_site(cfg.auth.as_ref())?,
        &cfg.site.name,
        &url,
        Arc::new(LogNotifier),
    ))
}

fn show(e: AuthError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

pub fn user_cmd(cmd: UserCmd) -> Result<()> {
    let site = match &cmd {
        UserCmd::Create { site, .. }
        | UserCmd::List { site }
        | UserCmd::ResetLink { site, .. }
        | UserCmd::Set { site, .. }
        | UserCmd::TotpOff { site, .. } => site.site.clone(),
    };
    let conn = connect(&site)?;
    conn.ensure_migrated()?;
    let auth = auth_for(&site, conn.db.clone())?;
    let rt = &conn.rt;
    match cmd {
        UserCmd::Create {
            email,
            name,
            mut roles,
            password_stdin,
            ..
        } => {
            let first = rt.block_on(auth.user_count())? == 0;
            if roles.is_empty() && first && cfg!(feature = "roles") {
                roles.push("admin".into());
            }
            let password = if password_stdin {
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .context("reading the password from standard input")?;
                let p = line.trim_end_matches(['\r', '\n']).to_string();
                if p.is_empty() {
                    bail!("--password-stdin: standard input was empty");
                }
                Some(p)
            } else {
                None
            };
            let u = rt
                .block_on(auth.create_user(&email, &name, password.as_deref(), &roles))
                .map_err(show)?;
            println!(
                "Created account #{} {}{}",
                u.id,
                u.email,
                if u.roles.is_empty() {
                    String::new()
                } else {
                    format!(" (roles: {})", u.roles.join(", "))
                }
            );
            if cfg!(feature = "roles") && u.roles.is_empty() {
                eprintln!("note: the account has no roles, so it cannot use the admin yet");
            }
            if password.is_none() {
                let link = rt.block_on(auth.reset_link(u.id))?;
                println!(
                    "Choose its password within one hour, once, at:\n  {link}\n(on a local server, \
                     replace the host with the address `engine serve` prints)"
                );
            }
        }
        UserCmd::List { .. } => {
            println!(
                "{:>4}  {:<32} {:<6} {:<4} {:<20} roles",
                "id", "email", "active", "2fa", "last sign-in"
            );
            for u in rt.block_on(auth.users())? {
                println!(
                    "{:>4}  {:<32} {:<6} {:<4} {:<20} {}",
                    u.id,
                    u.email,
                    if u.active { "yes" } else { "no" },
                    if u.totp_enabled { "on" } else { "off" },
                    u.last_login_at.as_deref().unwrap_or("never"),
                    u.roles.join(", ")
                );
            }
        }
        UserCmd::ResetLink { email, .. } => {
            let u = rt
                .block_on(auth.user_by_email(&email))?
                .with_context(|| format!("no account {email}"))?;
            println!("{}", rt.block_on(auth.reset_link(u.id))?);
        }
        UserCmd::Set {
            email,
            roles,
            no_roles,
            active,
            ..
        } => {
            let u = rt
                .block_on(auth.user_by_email(&email))?
                .with_context(|| format!("no account {email}"))?;
            let ch = UserChanges {
                active,
                roles: if no_roles {
                    Some(Vec::new())
                } else if roles.is_empty() {
                    None
                } else {
                    Some(roles)
                },
                ..Default::default()
            };
            let u = rt
                .block_on(auth.update_user(u.id, u.version, &ch))
                .map_err(show)?;
            println!(
                "Saved {}: {}, roles: {}",
                u.email,
                if u.active { "active" } else { "blocked" },
                if u.roles.is_empty() {
                    "none".into()
                } else {
                    u.roles.join(", ")
                }
            );
        }
        UserCmd::TotpOff { email, .. } => {
            let u = rt
                .block_on(auth.user_by_email(&email))?
                .with_context(|| format!("no account {email}"))?;
            rt.block_on(auth.disable_totp(u.id))?;
            println!("Two-factor sign-in is off for {}.", u.email);
        }
    }
    Ok(())
}
