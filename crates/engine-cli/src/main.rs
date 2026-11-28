//! `engine`: the Hammerwright command line.
//!
//! The stock binary is the empty app: a site crate builds its own binary
//! the same way, with its handlers, queries, jobs, hooks and commands
//! registered on [`engine_cli::App`] (docs: "App crates").

fn main() -> std::process::ExitCode {
    engine_cli::App::new().run()
}
