//! The Accounts components for site templates (`components/login_form`,
//! `register_form`, `reset_form`, `profile_form`, `account_menu`) and the
//! `accounts` template global.
//!
//! The components post to the engine's `/account/` routes, so a site page
//! can show sign-in or registration in its own layout. They need a
//! `request` route: forms carry a CSRF token made for each visitor
//! (signed-in visitors' from their session, others' from a cookie). In a
//! `build` or `publish` page, where no such token exists, they show a link
//! to the engine's own page instead.

use include_dir::{Dir, include_dir};
use minijinja::context;

use engine_core::builder::SiteExtension;

use crate::{AuthConfig, Registration};

/// Templates of the Accounts components.
pub static COMPONENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/site-templates");

/// Adds the Accounts components and the `accounts` global
/// (`registration`, `login_url`, `register_url`, `reset_url`,
/// `profile_url`, `logout_url`).
pub struct AccountsExtension {
    registration: bool,
}

impl AccountsExtension {
    pub fn new(cfg: &AuthConfig) -> Self {
        AccountsExtension {
            registration: cfg.registration == Registration::Open,
        }
    }
}

impl SiteExtension for AccountsExtension {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        env.add_global(
            "accounts",
            context! {
                registration => self.registration,
                login_url => "/account/login/",
                logout_url => "/account/logout/",
                register_url => "/account/register/",
                reset_url => "/account/reset/",
                profile_url => "/account/profile/",
            },
        );
    }

    fn template_dirs(&self) -> Vec<&'static Dir<'static>> {
        vec![&COMPONENTS]
    }
}
