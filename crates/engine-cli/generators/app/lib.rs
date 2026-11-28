//! __Label__: what this site adds to the engine (docs: "App crates").
//!
//! `engine generate handler|job|mailer` writes modules next to this file
//! and registers them in [`app`]; `src/main.rs` runs it.

use engine_cli::App;

/// Everything this site adds to the engine: handlers, named queries, jobs,
/// hooks, migrations and commands (see `engine_cli::App`).
pub fn app() -> App {
    App::new()
}
