//! `engine api token`: API tokens from the command line (scripts, first
//! setup). The admin has the same at `/admin/api-tokens/`.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use engine_api::{Scope, TokenStore};

use crate::SiteArg;
use crate::db::connect;

#[derive(Subcommand)]
pub enum ApiCmd {
    /// Manage API tokens.
    #[command(subcommand)]
    Token(TokenCmd),
}

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Make a token for an account and print it (shown only now).
    Create {
        #[command(flatten)]
        site: SiteArg,
        /// The account it acts as.
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: String,
        /// `read`, `write`, `read:<type>` or `write:<type>` (repeatable;
        /// default `read`).
        #[arg(long = "scope", value_name = "SCOPE")]
        scopes: Vec<String>,
        /// Lifetime in days (default: until revoked).
        #[arg(long)]
        days: Option<u32>,
    },
    /// List tokens (never their secrets).
    List {
        #[command(flatten)]
        site: SiteArg,
    },
    /// Revoke a token by number.
    Revoke {
        #[command(flatten)]
        site: SiteArg,
        id: i64,
    },
}

pub fn api_cmd(cmd: ApiCmd) -> Result<()> {
    let ApiCmd::Token(cmd) = cmd;
    match cmd {
        TokenCmd::Create {
            site,
            email,
            name,
            scopes,
            days,
        } => {
            let c = connect(&site.site)?;
            c.ensure_migrated()?;
            let auth = crate::user::auth_for(&site.site, c.db.clone())?;
            let user =
                c.rt.block_on(auth.user_by_email(&email))?
                    .with_context(|| format!("no account {email}"))?;
            let scopes: Vec<Scope> = if scopes.is_empty() {
                vec![Scope::parse("read").expect("valid")]
            } else {
                scopes
                    .iter()
                    .map(|s| Scope::parse(s).with_context(|| format!("`{s}` is not a scope")))
                    .collect::<Result<_>>()?
            };
            let store = TokenStore::new(c.db.clone());
            let n = match c.rt.block_on(store.create(user.id, &name, &scopes, days)) {
                Ok(n) => n,
                Err(e) => bail!("{e}"),
            };
            eprintln!(
                "Token #{} \"{}\" for {} ({}). It is shown only now:",
                n.token.id,
                n.token.name,
                user.email,
                scopes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("{}", n.secret);
        }
        TokenCmd::List { site } => {
            let c = connect(&site.site)?;
            c.ensure_migrated()?;
            let store = TokenStore::new(c.db.clone());
            for t in c.rt.block_on(store.all(1000))? {
                println!(
                    "#{:<4} {:<24} {}…  user {:<4} {:<30} {}",
                    t.id,
                    t.name,
                    t.prefix,
                    t.user_id,
                    t.scopes
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                    if t.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    }
                );
            }
        }
        TokenCmd::Revoke { site, id } => {
            let c = connect(&site.site)?;
            c.ensure_migrated()?;
            let store = TokenStore::new(c.db.clone());
            if c.rt.block_on(store.revoke(id))? {
                println!("Revoked token #{id}.");
            } else {
                bail!("no active token #{id}");
            }
        }
    }
    Ok(())
}
